use std::sync::Arc;

use domain::ports::storage::{StorageError, StoragePort};
use domain::ports::unit_of_work::UnitOfWork;
use domain::ports::video::{RepositoryError, VideoRepository};
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::metadata::process_video::ProcessVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{VideoId, VideoStatus, MAX_UPLOAD_SIZE_BYTES};

// All supported video formats have their magic bytes within the first 12 bytes.
// 16 gives a small safety margin without downloading more data than needed.
const FILE_SIGNATURE_READ_BYTES: u64 = 16;

pub struct CompleteUploadUseCase {
    video_repo: Arc<dyn VideoRepository>,
    uow: Arc<dyn UnitOfWork>,
    storage: Arc<dyn StoragePort>,
}

impl CompleteUploadUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        uow: Arc<dyn UnitOfWork>,
        storage: Arc<dyn StoragePort>,
    ) -> Self {
        Self { video_repo, uow, storage }
    }

    pub async fn execute(&self, input: Input) -> Result<Output, Error> {
        let video = self
            .video_repo
            .find_by_id(&input.id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        if video.status != VideoStatus::PendingUpload {
            return Err(Error::AlreadyCompleted);
        }

        let metadata = self
            .storage
            .head_object(&video.upload_key)
            .await
            .map_err(|e| match e {
                StorageError::NotFound(_) => Error::FileNotFoundInStorage,
                other => Error::Internal(other.to_string()),
            })?
            .ok_or(Error::FileNotFoundInStorage)?;

        if metadata.size_bytes > MAX_UPLOAD_SIZE_BYTES {
            // Schedule immediate deletion. The video row stays in
            // PENDING_UPLOAD until the DeleteVideo task runs. If the
            // schedule itself fails, the safety-net sweep (UC-VID-006)
            // picks it up within 24h — log and still return the user error.
            if let Err(e) = schedule_delete(&self.uow, &video.id).await {
                tracing::warn!(
                    video_id = %video.id,
                    error = %e,
                    "failed to schedule DeleteVideo after FileTooLarge rejection; safety-net sweep will collect",
                );
            }
            return Err(Error::FileTooLarge);
        }

        let header_bytes = self
            .storage
            .read_range(&video.upload_key, 0, FILE_SIGNATURE_READ_BYTES - 1)
            .await
            .map_err(|e| match e {
                StorageError::NotFound(_) => Error::FileNotFoundInStorage,
                other => Error::Internal(other.to_string()),
            })?;

        if !video.format.validate_signature(&header_bytes) {
            if let Err(e) = schedule_delete(&self.uow, &video.id).await {
                tracing::warn!(
                    video_id = %video.id,
                    error = %e,
                    "failed to schedule DeleteVideo after InvalidFileSignature rejection; safety-net sweep will collect",
                );
            }
            return Err(Error::InvalidFileSignature);
        }

        // Success path: atomic status update + ProcessVideo scheduling in one tx.
        // Per rule #8, the task must be scheduled in the same transaction as the
        // triggering business mutation. If the status update races another worker
        // (status no longer PENDING_UPLOAD), we roll back the whole tx — no stale
        // schedule.
        let mut tx = self
            .uow
            .begin()
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        let updated = tx
            .videos()
            .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !updated {
            // Don't commit — nothing changed. The dropped tx rolls back cleanly.
            return Err(Error::AlreadyCompleted);
        }

        TaskScheduler::schedule(
            tx.tasks(),
            &ProcessVideoTaskMetadata { video_id: video.id.clone() },
            None,
            None,
        )
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(Output {
            id: video.id,
            status: VideoStatus::Uploaded,
        })
    }
}

/// Opens a one-shot tx and schedules a `DeleteVideo` task for the given
/// video. Idempotent at the schedule level — repeated calls for the same
/// video while a previous task is still active return the existing task
/// (dedup-by-default via `TaskScheduler`).
async fn schedule_delete(
    uow: &Arc<dyn UnitOfWork>,
    video_id: &VideoId,
) -> Result<(), RepositoryError> {
    let mut tx = uow.begin().await?;
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

pub struct Input {
    pub id: VideoId,
}

pub struct Output {
    pub id: VideoId,
    pub status: VideoStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("video not found")]
    VideoNotFound,
    #[error("already completed")]
    AlreadyCompleted,
    #[error("file not found in storage")]
    FileNotFoundInStorage,
    #[error("file exceeds maximum size")]
    FileTooLarge,
    #[error("file signature does not match declared format")]
    InvalidFileSignature,
    #[error("internal error: {0}")]
    Internal(String),
}
