use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transaction::TransactionPort;
use domain::ports::transcoder::TranscoderPort;
use domain::ports::video::{RepositoryError, VideoRepository};
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{generate_share_token, Video, VideoId, VideoStatus};

pub struct ProcessVideoUseCase {
    video_repo: Arc<dyn VideoRepository>,
    tx: Arc<dyn TransactionPort>,
    storage: Arc<dyn StoragePort>,
    transcoder: Arc<dyn TranscoderPort>,
}

impl ProcessVideoUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        tx: Arc<dyn TransactionPort>,
        storage: Arc<dyn StoragePort>,
        transcoder: Arc<dyn TranscoderPort>,
    ) -> Self {
        Self { video_repo, tx, storage, transcoder }
    }

    pub async fn execute(&self, input: Input) -> Result<(), Error> {
        tracing::info!(video_id = %input.video_id, "starting video processing");

        let video = self
            .video_repo
            .find_by_id(&input.video_id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        // Atomically claim: UPLOADED -> PROCESSING. Single statement, no tx.
        let claimed = self
            .video_repo
            .update_status_if(&video.id, VideoStatus::Uploaded, VideoStatus::Processing)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !claimed {
            return Ok(()); // Another worker claimed it
        }

        // Probe — also captures has_audio so transcode_to_hls doesn't have
        // to re-read the file headers.
        let probe = match self.transcoder.probe(&video.upload_key).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(video_id = %video.id, error = %e, "probe failed");
                if let Err(tx_err) = fail_and_schedule_delete(&self.tx, &video.id).await {
                    tracing::error!(
                        video_id = %video.id,
                        error = %tx_err,
                        "failed to record probe-failure outcome; safety-net sweep will collect",
                    );
                }
                return Ok(());
            }
        };

        // Transcode — the on_first_segment callback signals via a oneshot
        // channel. A sibling task awaits the signal and does the share-token
        // + Processing→Partial transition. We MUST await the sibling before
        // doing any post-transcode status updates, otherwise a fast
        // transcode could finish before the sibling's writes complete and
        // the post-transcode `update_status_if(Partial → Processed)` would
        // see the row still in Processing — leaving the video stuck.
        //
        // The callback itself is synchronous (called from inside gstreamer's
        // spawn_blocking pipeline), so it can only fire-and-forget the
        // signal — it cannot await.
        let (first_segment_tx, first_segment_rx) = tokio::sync::oneshot::channel::<()>();
        let on_first_segment = {
            let mut tx_opt = Some(first_segment_tx);
            Box::new(move || {
                if let Some(tx) = tx_opt.take() {
                    let _ = tx.send(());
                }
            })
        };

        let signal_writer = {
            let repo = self.video_repo.clone();
            let id = video.id.clone();
            tokio::spawn(async move {
                if first_segment_rx.await.is_ok() {
                    let token = generate_share_token();
                    let _ = repo.set_share_token(&id, &token).await;
                    let _ = repo
                        .update_status_if(&id, VideoStatus::Processing, VideoStatus::Partial)
                        .await;
                    tracing::info!(video_id = %id, "first segment ready, share token generated");
                }
                // If `first_segment_rx.await` returned `Err`, the sender was
                // dropped without firing — the transcoder finished without
                // producing a first-segment callback. Nothing to do.
            })
        };

        let output_prefix = video.storage_prefix();
        let transcode_result = self
            .transcoder
            .transcode_to_hls(&video.upload_key, &output_prefix, &probe, on_first_segment)
            .await;

        // Wait for the signal writer to drain before any post-transcode
        // status update. This is the synchronization point that closes the
        // race window. If the writer panicked, log and continue — the
        // post-transcode update will still try to do its job.
        if let Err(e) = signal_writer.await {
            tracing::error!(
                video_id = %video.id,
                error = %e,
                "first-segment writer task failed",
            );
        }

        match transcode_result {
            Ok(result) => {
                if result.segments_produced > 0 {
                    // All segments produced → PROCESSED. Keep HLS output,
                    // delete the original upload inline (best-effort).
                    let _ = self
                        .video_repo
                        .update_status_if(&video.id, VideoStatus::Partial, VideoStatus::Processed)
                        .await;
                    cleanup_original_inline(&self.storage, &video).await;
                } else {
                    // Zero segments → FAILED + schedule DeleteVideo.
                    // One tx covers both mutations.
                    if let Err(tx_err) = fail_and_schedule_delete(&self.tx, &video.id).await {
                        tracing::error!(
                            video_id = %video.id,
                            error = %tx_err,
                            "failed to record zero-segments outcome; safety-net sweep will collect",
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(video_id = %video.id, error = %e, "transcode failed");
                // Check if we produced any segments (PARTIAL status set by
                // the on_first_segment callback).
                let current = self
                    .video_repo
                    .find_by_id(&video.id)
                    .await
                    .map_err(|e| Error::Internal(e.to_string()))?;
                if let Some(v) = current {
                    if v.status == VideoStatus::Partial {
                        // Partial success → INCOMPLETE. Keep the segments
                        // we have, delete the original upload inline.
                        let _ = self
                            .video_repo
                            .update_status_if(&video.id, VideoStatus::Partial, VideoStatus::Incomplete)
                            .await;
                        cleanup_original_inline(&self.storage, &video).await;
                    } else {
                        // No segments produced → FAILED + schedule DeleteVideo.
                        if let Err(tx_err) =
                            fail_and_schedule_delete(&self.tx, &video.id).await
                        {
                            tracing::error!(
                                video_id = %video.id,
                                error = %tx_err,
                                "failed to record transcode-failure outcome; safety-net sweep will collect",
                            );
                        }
                    }
                }
            }
        }

        tracing::info!(video_id = %video.id, "processing complete");
        Ok(())
    }
}

/// Atomically transition `Processing → Failed` and schedule a `DeleteVideo`
/// task in one transaction. Used by the probe-failure and zero-segments
/// paths. If either mutation fails, both roll back and the safety-net
/// sweep eventually collects the video.
async fn fail_and_schedule_delete(
    tx: &Arc<dyn TransactionPort>,
    video_id: &VideoId,
) -> Result<(), RepositoryError> {
    let id = video_id.clone();
    tx.run(Box::new(move |scope| {
        Box::pin(async move {
            scope
                .videos()
                .update_status_if(&id, VideoStatus::Processing, VideoStatus::Failed)
                .await?;
            TaskScheduler::schedule_in_tx(
                scope.tasks(),
                &DeleteVideoTaskMetadata { video_id: id.clone() },
                None,
            )
            .await?;
            Ok(())
        })
    }))
    .await
}

/// Best-effort inline deletion of the original upload for streamable
/// outcomes (PROCESSED, INCOMPLETE). Failure leaves an orphan for the
/// safety-net sweep to collect.
async fn cleanup_original_inline(storage: &Arc<dyn StoragePort>, video: &Video) {
    if let Err(e) = storage.delete_object(&video.upload_key).await {
        tracing::warn!(
            video_id = %video.id,
            upload_key = %video.upload_key,
            error = %e,
            "failed to delete original upload after processing; orphan will be collected by cleanup task",
        );
    }
}

pub struct Input {
    pub video_id: VideoId,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("video not found")]
    VideoNotFound,
    #[error("internal error: {0}")]
    Internal(String),
}
