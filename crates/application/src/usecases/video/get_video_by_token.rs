use std::sync::Arc;

use domain::ports::video::VideoRepository;

pub struct GetVideoByTokenUseCase {
    video_repo: Arc<dyn VideoRepository>,
    cdn_base_url: String,
}

impl GetVideoByTokenUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, cdn_base_url: String) -> Self {
        Self { video_repo, cdn_base_url }
    }

    pub async fn execute(&self, input: Input) -> Result<Output, Error> {
        let video = self
            .video_repo
            .find_by_share_token(&input.share_token)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        let stream_url = video.stream_url(self.cdn_base_url.trim_end_matches('/'));

        Ok(Output {
            title: video.title,
            stream_url,
        })
    }
}

pub struct Input {
    pub share_token: String,
}

pub struct Output {
    pub title: String,
    pub stream_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("video not found")]
    VideoNotFound,
    #[error("internal error: {0}")]
    Internal(String),
}
