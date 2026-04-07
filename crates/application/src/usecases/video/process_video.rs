use std::sync::Arc;

use domain::ports::error::RepositoryError;
use domain::ports::storage::StoragePort;
use domain::ports::transaction::TransactionPort;
use domain::ports::transcoder::TranscoderPort;
use domain::ports::video::VideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{generate_share_token, Video, VideoId, VideoStatus};

/// UC-VID-005 — Process Video.
///
/// Lifecycle: claim (`Uploaded → Processing`) → probe → transcode →
/// `mark_processed` (sets share token + `Processing → Processed` in one
/// statement). On any failure, transition to `Failed` and schedule a
/// `DeleteVideo` task in the same transaction.
///
/// Probe + the upload-time validations (client-side type/size check, server
/// magic-byte signature check) provide strong evidence the file is good
/// before we start transcoding. We don't try to be partially-resilient on
/// transcode failures: if the pipeline errors out, the whole video is
/// marked failed and scheduled for deletion.
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

        // Probe — validates the file is decodable AND captures `has_audio`
        // so the transcoder doesn't need to re-read the headers.
        let probe = match self.transcoder.probe(&video.upload_key).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(video_id = %video.id, error = %e, "probe failed");
                self.fail(&video.id, "probe failed").await;
                return Ok(());
            }
        };

        // Transcode (parallel pipeline, runs to completion).
        let output_prefix = video.storage_prefix();
        if let Err(e) = self
            .transcoder
            .transcode_to_hls(&video.upload_key, &output_prefix, &probe)
            .await
        {
            tracing::error!(video_id = %video.id, error = %e, "transcode failed");
            self.fail(&video.id, "transcode failed").await;
            return Ok(());
        }

        // Success: set share_token + status=PROCESSED in one statement.
        let token = generate_share_token();
        match self.video_repo.mark_processed(&video.id, &token).await {
            Ok(true) => {
                cleanup_original_inline(&self.storage, &video).await;
                tracing::info!(video_id = %video.id, "processing complete");
            }
            Ok(false) => {
                // Status was no longer Processing — recovered out from
                // under us by something else. Nothing to do.
                tracing::warn!(
                    video_id = %video.id,
                    "mark_processed found no row in Processing state",
                );
            }
            Err(e) => {
                tracing::error!(
                    video_id = %video.id,
                    error = %e,
                    "failed to mark video processed; safety-net sweep will collect",
                );
            }
        }

        Ok(())
    }

    async fn fail(&self, video_id: &VideoId, reason: &str) {
        if let Err(tx_err) = fail_and_schedule_delete(&self.tx, video_id).await {
            tracing::error!(
                video_id = %video_id,
                reason,
                error = %tx_err,
                "failed to record failure outcome; safety-net sweep will collect",
            );
        }
    }
}

/// Atomically transition `Processing → Failed` and schedule a `DeleteVideo`
/// task in one transaction. If either mutation fails, both roll back and
/// the safety-net sweep eventually collects the video.
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

/// Best-effort inline deletion of the original upload after a successful
/// transcode. Failure leaves an orphan for the safety-net sweep to collect.
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
