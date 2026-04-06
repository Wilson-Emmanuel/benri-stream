use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::video::{Video, VideoId};

/// Read-only video repository. Mutations live on `VideoMutations` in
/// `crate::ports::unit_of_work` and must be performed inside a `TxScope`.
#[async_trait]
pub trait VideoRepository: Send + Sync {
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError>;
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError>;

    /// Find videos in transient states older than threshold (for cleanup).
    async fn find_stale(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;

    /// Find FAILED videos older than threshold (for cleanup).
    async fn find_failed_before(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(String),
}
