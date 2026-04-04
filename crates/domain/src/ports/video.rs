use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::video::{Video, VideoId, VideoStatus};

#[async_trait]
pub trait VideoRepository: Send + Sync {
    async fn insert(&self, video: &Video) -> Result<(), RepositoryError>;
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError>;
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError>;

    /// Atomically set status only if current status matches `expected`. Returns true if updated.
    async fn update_status_if(
        &self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError>;

    async fn set_share_token(&self, id: &VideoId, token: &str) -> Result<(), RepositoryError>;

    /// Find videos in transient states older than threshold (for cleanup).
    async fn find_stale(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;

    /// Find FAILED videos older than threshold (for cleanup).
    async fn find_failed_before(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;

    async fn delete(&self, id: &VideoId) -> Result<(), RepositoryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(String),
}
