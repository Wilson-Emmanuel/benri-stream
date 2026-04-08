use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::ports::error::RepositoryError;
use crate::video::{Video, VideoId, VideoStatus};

/// Pool-backed video operations. These are single-statement writes and
/// reads that are atomic by themselves and have no need to be bundled
/// with other mutations. Multi-statement atomic writes (e.g. update status
/// + schedule task) go through
/// [`crate::ports::transaction::TransactionPort`] instead.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait VideoRepository: Send + Sync {
    // ---- Reads ----
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError>;
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError>;

    /// Find videos in transient states older than threshold (for cleanup).
    async fn find_stale(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;

    /// Find FAILED videos older than threshold (for cleanup).
    async fn find_failed_before(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError>;

    // ---- Single-statement writes ----
    async fn insert(&self, video: &Video) -> Result<(), RepositoryError>;

    /// Atomically set status only if current status matches `expected`.
    /// Returns `true` if a row was updated.
    async fn update_status_if(
        &self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError>;

    /// Atomically transition `Processing → Processed` and write the
    /// share token in one statement. Returns `true` if a row was updated;
    /// `false` means the video was no longer in `Processing` (e.g.
    /// recovered to `Failed` by the safety-net sweep). Single statement,
    /// no transaction needed.
    async fn mark_processed(
        &self,
        id: &VideoId,
        share_token: &str,
    ) -> Result<bool, RepositoryError>;

    async fn delete(&self, id: &VideoId) -> Result<(), RepositoryError>;

    // ---- Bulk writes ----

    /// Bulk transition videos to FAILED. Only updates rows whose current
    /// status is one of `from_statuses`. Used by the cleanup sweep to mark
    /// stuck UPLOADED / PROCESSING videos.
    async fn bulk_mark_failed(
        &self,
        ids: &[VideoId],
        from_statuses: &[VideoStatus],
    ) -> Result<(), RepositoryError>;
}
