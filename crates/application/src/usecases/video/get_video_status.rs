use std::sync::Arc;

use domain::ports::video::VideoRepository;
use domain::video::{VideoId, VideoStatus};

pub struct GetVideoStatusUseCase {
    video_repo: Arc<dyn VideoRepository>,
    base_url: String,
}

impl GetVideoStatusUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, base_url: String) -> Self {
        Self { video_repo, base_url }
    }

    pub async fn execute(&self, input: Input) -> Result<Output, Error> {
        let video = self
            .video_repo
            .find_by_id(&input.id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        Ok(Output {
            status: video.status,
            share_url: video.share_url(&self.base_url),
        })
    }
}

pub struct Input {
    pub id: VideoId,
}

pub struct Output {
    pub status: VideoStatus,
    pub share_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("video not found")]
    VideoNotFound,
    #[error("internal error: {0}")]
    Internal(String),
}
