use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::video::VideoRepository;
use domain::video::VideoId;

/// UC-VID-007 Delete Video.
///
/// Single delete path for a video's storage objects and database record.
/// Idempotent — running on a missing video or partially-deleted state is
/// a no-op at each step.
///
/// Mutations are strictly ordered: HLS output first, then the original upload,
/// then the database row. If any step fails, later steps do not run and the
/// caller (task handler) converts the error into `RetryableFailure` so the
/// next attempt re-runs the remaining steps. Both storage operations tolerate
/// "already deleted" outcomes, so retries are safe.
pub struct DeleteVideoUseCase {
    video_repo: Arc<dyn VideoRepository>,
    storage: Arc<dyn StoragePort>,
}

impl DeleteVideoUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, storage: Arc<dyn StoragePort>) -> Self {
        Self { video_repo, storage }
    }

    pub async fn execute(&self, input: Input) -> Result<(), Error> {
        tracing::info!(video_id = %input.video_id, "deleting video");

        let video = self
            .video_repo
            .find_by_id(&input.video_id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        let Some(video) = video else {
            // Already deleted — no-op. Handler converts this to Skip.
            return Err(Error::VideoNotFound);
        };

        // 1. HLS output tree — no-op if prefix is empty (e.g. probe-failed videos).
        self.storage
            .delete_prefix(&video.storage_prefix())
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // 2. Original upload — no-op if already deleted by the success path.
        self.storage
            .delete_object(&video.upload_key)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // 3. Database row.
        self.video_repo
            .delete(&video.id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        tracing::info!(video_id = %video.id, "video deleted");
        Ok(())
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
