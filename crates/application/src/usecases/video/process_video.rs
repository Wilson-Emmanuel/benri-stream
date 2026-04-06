use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::TranscoderPort;
use domain::ports::unit_of_work::UnitOfWork;
use domain::ports::video::{RepositoryError, VideoRepository};
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{generate_share_token, Video, VideoId, VideoStatus};

pub struct ProcessVideoUseCase {
    video_repo: Arc<dyn VideoRepository>,
    uow: Arc<dyn UnitOfWork>,
    storage: Arc<dyn StoragePort>,
    transcoder: Arc<dyn TranscoderPort>,
}

impl ProcessVideoUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        uow: Arc<dyn UnitOfWork>,
        storage: Arc<dyn StoragePort>,
        transcoder: Arc<dyn TranscoderPort>,
    ) -> Self {
        Self { video_repo, uow, storage, transcoder }
    }

    pub async fn execute(&self, input: Input) -> Result<(), Error> {
        let video = self
            .video_repo
            .find_by_id(&input.video_id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        // Atomically claim: UPLOADED -> PROCESSING
        let claimed = update_status(
            &self.uow,
            &video.id,
            VideoStatus::Uploaded,
            VideoStatus::Processing,
        )
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;

        if !claimed {
            return Ok(()); // Another worker claimed it
        }

        // Probe
        if let Err(e) = self.transcoder.probe(&video.upload_key).await {
            tracing::error!(video_id = %video.id, error = %e, "probe failed");
            if let Err(tx_err) = fail_and_schedule_delete(&self.uow, &video.id).await {
                tracing::error!(
                    video_id = %video.id,
                    error = %tx_err,
                    "failed to record probe-failure outcome; safety-net sweep will collect",
                );
            }
            return Ok(());
        }

        // Transcode — the on_first_segment callback generates the share token
        let uow = self.uow.clone();
        let video_id = video.id.clone();

        let on_first_segment = Box::new(move || {
            let uow = uow;
            let id = video_id;
            let token = generate_share_token();
            tokio::spawn(async move {
                let _ = set_share_token(&uow, &id, &token).await;
                let _ = update_status(&uow, &id, VideoStatus::Processing, VideoStatus::Partial).await;
                tracing::info!(video_id = %id, "first segment ready, share token generated");
            });
        });

        let output_prefix = video.storage_prefix();
        match self
            .transcoder
            .transcode_to_hls(&video.upload_key, &output_prefix, on_first_segment)
            .await
        {
            Ok(result) => {
                if result.segments_produced > 0 {
                    // All segments produced → PROCESSED. Keep HLS output,
                    // delete the original upload inline (best-effort).
                    let _ = update_status(
                        &self.uow,
                        &video.id,
                        VideoStatus::Partial,
                        VideoStatus::Processed,
                    )
                    .await;
                    cleanup_original_inline(&self.storage, &video).await;
                } else {
                    // Zero segments → FAILED + schedule DeleteVideo.
                    // One tx covers both mutations.
                    if let Err(tx_err) = fail_and_schedule_delete(&self.uow, &video.id).await {
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
                        let _ = update_status(
                            &self.uow,
                            &video.id,
                            VideoStatus::Partial,
                            VideoStatus::Incomplete,
                        )
                        .await;
                        cleanup_original_inline(&self.storage, &video).await;
                    } else {
                        // No segments produced → FAILED + schedule DeleteVideo.
                        if let Err(tx_err) =
                            fail_and_schedule_delete(&self.uow, &video.id).await
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

/// One-shot transactional status update.
async fn update_status(
    uow: &Arc<dyn UnitOfWork>,
    id: &VideoId,
    expected: VideoStatus,
    new_status: VideoStatus,
) -> Result<bool, RepositoryError> {
    let mut tx = uow.begin().await?;
    let ok = tx.videos().update_status_if(id, expected, new_status).await?;
    tx.commit().await?;
    Ok(ok)
}

async fn set_share_token(
    uow: &Arc<dyn UnitOfWork>,
    id: &VideoId,
    token: &str,
) -> Result<(), RepositoryError> {
    let mut tx = uow.begin().await?;
    tx.videos().set_share_token(id, token).await?;
    tx.commit().await?;
    Ok(())
}

/// Atomically transition `Processing → Failed` and schedule a `DeleteVideo`
/// task in one transaction. Used by the probe-failure and zero-segments
/// paths. If either mutation fails, both roll back and the safety-net
/// sweep eventually collects the video.
async fn fail_and_schedule_delete(
    uow: &Arc<dyn UnitOfWork>,
    video_id: &VideoId,
) -> Result<(), RepositoryError> {
    let mut tx = uow.begin().await?;
    tx.videos()
        .update_status_if(video_id, VideoStatus::Processing, VideoStatus::Failed)
        .await?;
    TaskScheduler::schedule(
        tx.tasks(),
        &DeleteVideoTaskMetadata { video_id: video_id.clone() },
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
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
