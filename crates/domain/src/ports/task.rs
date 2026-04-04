use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::task::{Task, TaskId, TaskUpdate};
use crate::ports::video::RepositoryError;

#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn create(&self, task: &Task) -> Result<Task, RepositoryError>;
    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError>;
    async fn reset_stale(&self, threshold: DateTime<Utc>) -> Result<i32, RepositoryError>;
    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError>;
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
