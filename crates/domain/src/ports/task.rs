use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::ports::error::RepositoryError;
use crate::task::{Task, TaskId, TaskUpdate};

/// Pool-backed task operations. Reads, lifecycle management, and single
/// or bulk inserts that don't need to be bundled with other writes.
/// Task creation that must be atomic with a business mutation goes through
/// [`crate::ports::transaction::TransactionPort`] + `TaskMutations`
/// instead.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError>;
    async fn reset_stale(&self) -> Result<i32, RepositoryError>;
    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError>;

    /// Insert one task. Single statement, atomic on its own. Use when
    /// scheduling a task with no business mutation to bundle with
    /// (system tasks, fire-and-forget schedules after a rejection).
    async fn create(&self, task: &Task) -> Result<Task, RepositoryError>;

    /// Bulk-insert N tasks in a single statement. Used by the cleanup sweep
    /// to schedule many DeleteVideo tasks at once.
    async fn bulk_create(&self, tasks: &[Task]) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait TaskPublisher: Send + Sync {
    async fn publish(&self, task_ids: &[TaskId]) -> Result<bool, QueueError>;
}

#[async_trait]
pub trait TaskConsumer: Send + Sync {
    async fn pop(&self) -> Result<Option<TaskId>, QueueError>;
}

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("queue error: {0}")]
    Internal(String),
}
