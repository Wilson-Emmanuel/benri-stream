use async_trait::async_trait;

use crate::ports::video::RepositoryError;
use crate::task::Task;
use crate::video::{Video, VideoId, VideoStatus};

/// Opens a database transaction scoped to a set of mutations that must commit
/// or rollback atomically. Use cases own transactions — see project rule #6.
///
/// Typical usage:
/// ```ignore
/// let mut tx = uow.begin().await?;
/// tx.videos().update_status_if(&id, expected, new_status).await?;
/// tx.tasks().create(&task).await?;
/// tx.commit().await?;
/// ```
///
/// If the `TxScope` is dropped without calling `commit`, the underlying
/// transaction is rolled back automatically.
#[async_trait]
pub trait UnitOfWork: Send + Sync {
    async fn begin(&self) -> Result<Box<dyn TxScope>, RepositoryError>;
}

/// A single open transaction. Exposes mutation traits scoped to this transaction.
///
/// `videos()` and `tasks()` each borrow `&mut self`; you can only hold one at a
/// time, which matches the sequential nature of a database transaction.
#[async_trait]
pub trait TxScope: Send {
    fn videos(&mut self) -> &mut dyn VideoMutations;
    fn tasks(&mut self) -> &mut dyn TaskMutations;

    /// Commits the transaction. After calling, the `TxScope` must not be used
    /// for further mutations. Dropping without calling this rolls back.
    async fn commit(&mut self) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait VideoMutations: Send {
    async fn insert(&mut self, video: &Video) -> Result<(), RepositoryError>;

    /// Atomically set status only if current status matches `expected`.
    /// Returns true if updated.
    async fn update_status_if(
        &mut self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError>;

    async fn set_share_token(&mut self, id: &VideoId, token: &str) -> Result<(), RepositoryError>;

    async fn delete(&mut self, id: &VideoId) -> Result<(), RepositoryError>;
}

/// Task operations performed inside a `TxScope`. Despite the name, this also
/// contains the `find_active_by_ordering_key` read — that read must run in the
/// same transaction as the subsequent `create` call to see uncommitted
/// concurrent inserts, which is why it lives here instead of on `TaskRepository`.
#[async_trait]
pub trait TaskMutations: Send {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError>;

    /// Returns the active (`PENDING` or `IN_PROGRESS`) task with the given
    /// `(metadata_type, ordering_key)` pair, if any. Used by `TaskScheduler`
    /// for dedup-by-default scheduling.
    async fn find_active_by_ordering_key(
        &mut self,
        metadata_type: &str,
        ordering_key: &str,
    ) -> Result<Option<Task>, RepositoryError>;
}
