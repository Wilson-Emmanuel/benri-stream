use std::sync::Arc;

use chrono::Utc;
use domain::ports::storage::StoragePort;
use domain::ports::video::VideoRepository;
use domain::video::{
    Video, VideoFormat, VideoId, VideoStatus, MAX_TITLE_LENGTH, MAX_UPLOAD_SIZE_BYTES,
};

pub struct InitiateUploadUseCase {
    video_repo: Arc<dyn VideoRepository>,
    storage: Arc<dyn StoragePort>,
}

impl InitiateUploadUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, storage: Arc<dyn StoragePort>) -> Self {
        Self { video_repo, storage }
    }

    pub async fn execute(&self, input: Input) -> Result<Output, Error> {
        let title = input.title.trim().to_string();
        let title_chars = title.chars().count();

        tracing::info!(
            mime_type = %input.mime_type,
            title_chars,
            "initiating upload",
        );

        if title.is_empty() {
            return Err(Error::TitleRequired);
        }
        // Spec says "1–100 chars", not bytes. `String::len` returns bytes,
        // so a 100-emoji title would be ~400 bytes and wrongly rejected.
        if title_chars > MAX_TITLE_LENGTH {
            return Err(Error::TitleTooLong);
        }

        let format = VideoFormat::from_mime_type(&input.mime_type)
            .ok_or(Error::UnsupportedFormat)?;

        let id = VideoId::new();
        let upload_key = format!("uploads/{}/original{}", id.0, format.extension());

        let video = Video {
            id: id.clone(),
            share_token: None,
            title,
            format,
            status: VideoStatus::PendingUpload,
            upload_key: upload_key.clone(),
            created_at: Utc::now(),
        };

        // Single-statement insert — no transaction needed.
        self.video_repo
            .insert(&video)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        let presigned = self
            .storage
            .generate_presigned_upload_url(&upload_key, &input.mime_type, MAX_UPLOAD_SIZE_BYTES, 1800)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(Output {
            id: video.id,
            upload_url: presigned.url,
        })
    }
}

pub struct Input {
    pub title: String,
    pub mime_type: String,
}

pub struct Output {
    pub id: VideoId,
    pub upload_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("title is required")]
    TitleRequired,
    #[error("title exceeds maximum length")]
    TitleTooLong,
    #[error("unsupported video format")]
    UnsupportedFormat,
    #[error("internal error: {0}")]
    Internal(String),
}
