use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use domain::ports::storage::{StorageError, StoragePort};
use domain::ports::task::TaskRepository;
use domain::ports::transaction::TransactionPort;
use domain::ports::video::VideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::metadata::process_video::ProcessVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{VideoId, VideoStatus, MAX_UPLOAD_SIZE_BYTES};

// All supported video formats have their magic bytes within the first 12 bytes.
// 16 gives a small safety margin without downloading more data than needed.
const FILE_SIGNATURE_READ_BYTES: u64 = 16;

pub struct CompleteUploadUseCase {
    video_repo: Arc<dyn VideoRepository>,
    task_repo: Arc<dyn TaskRepository>,
    tx: Arc<dyn TransactionPort>,
    storage: Arc<dyn StoragePort>,
}

impl CompleteUploadUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        task_repo: Arc<dyn TaskRepository>,
        tx: Arc<dyn TransactionPort>,
        storage: Arc<dyn StoragePort>,
    ) -> Self {
        Self { video_repo, task_repo, tx, storage }
    }

    pub async fn execute(&self, input: Input) -> Result<Output, Error> {
        tracing::info!(video_id = %input.id, "completing upload");

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
            schedule_delete_after_rejection(
                self.task_repo.as_ref(),
                &video.id,
                "FileTooLarge",
            )
            .await;
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
            schedule_delete_after_rejection(
                self.task_repo.as_ref(),
                &video.id,
                "InvalidFileSignature",
            )
            .await;
            return Err(Error::InvalidFileSignature);
        }

        // Success path: atomic status update + ProcessVideo scheduling in
        // one tx. The task must be scheduled in the same transaction as
        // the triggering business mutation — if the status update races
        // another worker (status no longer PENDING_UPLOAD), the whole tx
        // rolls back and no stale task is left behind.
        //
        // `claimed` is an owned `Arc<AtomicBool>` so the closure can be
        // `'static` while still allowing the result to be read after the
        // tx commits.
        let claimed = Arc::new(AtomicBool::new(false));
        let claimed_w = claimed.clone();
        let id_for_tx = video.id.clone();

        self.tx
            .run(Box::new(move |scope| {
                Box::pin(async move {
                    let ok = scope
                        .videos()
                        .update_status_if(&id_for_tx, VideoStatus::PendingUpload, VideoStatus::Uploaded)
                        .await?;
                    claimed_w.store(ok, Ordering::Relaxed);
                    if !ok {
                        // Return Ok — the tx commits with no changes.
                        // The caller sees `claimed == false` and returns
                        // AlreadyCompleted without scheduling the task.
                        return Ok(());
                    }
                    TaskScheduler::schedule_in_tx(
                        scope.tasks(),
                        &ProcessVideoTaskMetadata { video_id: id_for_tx.clone() },
                        None,
                    )
                    .await?;
                    Ok(())
                })
            }))
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !claimed.load(Ordering::Relaxed) {
            return Err(Error::AlreadyCompleted);
        }

        Ok(Output {
            id: video.id,
            status: VideoStatus::Uploaded,
        })
    }
}

/// Schedule a `DeleteVideo` task standalone (no transaction, no business
/// mutation to bundle with — the rejected video stays in `PENDING_UPLOAD`
/// and the task cleans it up). If the schedule itself fails, log it and
/// let the safety-net sweep (UC-VID-006) collect the orphaned video later.
async fn schedule_delete_after_rejection(
    task_repo: &dyn TaskRepository,
    video_id: &VideoId,
    rejection: &'static str,
) {
    if let Err(e) = TaskScheduler::schedule_standalone(
        task_repo,
        &DeleteVideoTaskMetadata { video_id: video_id.clone() },
        None,
    )
    .await
    {
        tracing::warn!(
            video_id = %video_id,
            rejection,
            error = %e,
            "failed to schedule DeleteVideo after rejection; safety-net sweep will collect",
        );
    }
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
