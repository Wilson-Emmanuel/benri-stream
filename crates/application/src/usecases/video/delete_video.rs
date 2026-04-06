use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::unit_of_work::UnitOfWork;
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
    uow: Arc<dyn UnitOfWork>,
    storage: Arc<dyn StoragePort>,
}

impl DeleteVideoUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        uow: Arc<dyn UnitOfWork>,
        storage: Arc<dyn StoragePort>,
    ) -> Self {
        Self { video_repo, uow, storage }
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

        // 1. Delete the HLS output tree. No-op if prefix is empty
        //    (e.g. probe-failed videos that never produced segments).
        self.storage
            .delete_prefix(&video.storage_prefix())
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // 2. Delete the original upload. No-op if the object is already gone
        //    (e.g. successful processing already deleted it inline).
        self.storage
            .delete_object(&video.upload_key)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // 3. Delete the database row.
        let mut tx = self
            .uow
            .begin()
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        tx.videos()
            .delete(&video.id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        tx.commit()
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
    /// The video does not exist (likely already deleted). The task handler
    /// maps this to `TaskResult::Skip` so the task completes cleanly.
    #[error("video not found")]
    VideoNotFound,

    /// Any infrastructure failure (storage, database). The task handler maps
    /// this to `TaskResult::RetryableFailure` so the task system retries with
    /// backoff and eventually dead-letters.
    #[error("internal error: {0}")]
    Internal(String),
}
