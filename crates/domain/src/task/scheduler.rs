use chrono::{DateTime, Utc};

use crate::ports::error::RepositoryError;
use crate::ports::task::TaskRepository;
use crate::ports::transaction::TaskMutations;
use super::trace_context;
use super::{Task, TaskId, TaskMetadata, TaskStatus};

/// Stateless entry point for creating tasks.
///
/// No deduplication — multiple calls for the same logical task create multiple
/// rows. Handlers must be idempotent. For resource serialization (preventing
/// concurrent runs on the same resource), use `TaskMetadata::ordering_key`.
pub struct TaskScheduler;

impl TaskScheduler {
    /// Builds a `Task` row in `Pending` state without inserting it.
    ///
    /// All scheduling paths go through here so the shape of a new row
    /// is defined in one place. `run_at` defaults to `now` when `None`.
    /// The trace id is read from the ambient [`trace_context::current_trace_id`]
    /// so tasks created inside a `with_trace_id` scope inherit the caller's id.
    pub fn build_pending_task<M: TaskMetadata>(
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let now = Utc::now();
        let metadata_json = serde_json::to_value(metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(Task {
            id: TaskId::new(),
            metadata_type: metadata.metadata_type_name().to_string(),
            metadata: metadata_json,
            status: TaskStatus::Pending,
            ordering_key: metadata.ordering_key(),
            trace_id: trace_context::current_trace_id(),
            attempt_count: 0,
            next_run_at: run_at.unwrap_or(now),
            error: None,
            started_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Inserts a task inside an open transaction, atomic with any other
    /// writes in that transaction.
    pub async fn schedule_in_tx<M: TaskMetadata>(
        tasks: &mut dyn TaskMutations,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let task = Self::build_pending_task(metadata, run_at)?;
        tasks.create(&task).await
    }

    /// Inserts a task directly against the pool, without an enclosing transaction.
    pub async fn schedule_standalone<M: TaskMetadata>(
        repo: &dyn TaskRepository,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let task = Self::build_pending_task(metadata, run_at)?;
        repo.create(&task).await
    }
}
