use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::video::VideoRepository;
use domain::video::{VideoId, VideoStatus, MAX_UPLOAD_SIZE_BYTES};

pub struct CompleteUploadUseCase {
    video_repo: Arc<dyn VideoRepository>,
    storage: Arc<dyn StoragePort>,
}

impl CompleteUploadUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, storage: Arc<dyn StoragePort>) -> Self {
        Self { video_repo, storage }
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
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::FileNotFoundInStorage)?;

        if metadata.size_bytes > MAX_UPLOAD_SIZE_BYTES {
            let _ = self.storage.delete_object(&video.upload_key).await;
            let _ = self.video_repo.delete(&video.id).await;
            return Err(Error::FileTooLarge);
        }

        let header_bytes = self
            .storage
            .read_range(&video.upload_key, 0, 4095)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !video.format.validate_signature(&header_bytes) {
            let _ = self.storage.delete_object(&video.upload_key).await;
            let _ = self.video_repo.delete(&video.id).await;
            return Err(Error::InvalidFileSignature);
        }

        let updated = self
            .video_repo
            .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !updated {
            return Err(Error::AlreadyCompleted);
        }

        Ok(Output {
            id: video.id,
            status: VideoStatus::Uploaded,
        })
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
