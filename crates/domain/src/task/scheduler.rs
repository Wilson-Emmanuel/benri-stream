use chrono::{DateTime, Utc};

use crate::ports::error::RepositoryError;
use crate::ports::task::TaskRepository;
use crate::ports::transaction::TaskMutations;
use super::{Task, TaskId, TaskMetadata, TaskStatus};

/// Stateless entry point for creating tasks.
///
/// **No deduplication.** Multiple `schedule*` calls for the same logical
/// task may create multiple rows. Handlers MUST be idempotent (see
/// `task-system.md` "Handler Idempotency"). For resource serialization
/// (preventing two handlers from running concurrently on the same
/// resource), use `TaskMetadata::ordering_key`.
pub struct TaskScheduler;

impl TaskScheduler {
    /// Construct a Task row in the PENDING state without inserting it.
    /// This is the **single source of truth** for how a TaskMetadata
    /// becomes a Task row. Both `schedule_in_tx` / `schedule_standalone`
    /// and bulk callers (like the cleanup sweep that uses
    /// `TaskRepository::bulk_create`) go through this helper so they
    /// can never drift.
    ///
    /// `run_at` defaults to `now` when `None`.
    ///
    /// **trace_id**: not currently propagated. The Task is created with
    /// `trace_id: None`. When OpenTelemetry / a tracing-context port is
    /// wired, populate it here from the current span.
    pub fn build_pending_task<M: TaskMetadata>(
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Task {
        let now = Utc::now();
        // `serde_json::to_value` only fails for `Serialize` impls that
        // explicitly return an error — none of our `TaskMetadata` impls do.
        // A failure here would be a programming bug in a metadata struct,
        // not a runtime condition, so panic instead of returning an error.
        let metadata_json = serde_json::to_value(metadata)
            .expect("TaskMetadata must serialize to a JSON value");

        Task {
            id: TaskId::new(),
            metadata_type: metadata.metadata_type_name().to_string(),
            metadata: metadata_json,
            status: TaskStatus::Pending,
            ordering_key: metadata.ordering_key(),
            trace_id: None,
            attempt_count: 0,
            next_run_at: run_at.unwrap_or(now),
            error: None,
            started_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Schedule a task inside an open transaction. Use when the schedule
    /// must be atomic with a business mutation — e.g. the use case updates
    /// a row and wants the task to exist if and only if the row update
    /// commits.
    pub async fn schedule_in_tx<M: TaskMetadata>(
        tasks: &mut dyn TaskMutations,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let task = Self::build_pending_task(metadata, run_at);
        tasks.create(&task).await
    }

    /// Schedule a task standalone, without a transaction. Use when there
    /// is no business mutation to bundle with — system tasks, recurring
    /// schedules, fire-and-forget retries.
    pub async fn schedule_standalone<M: TaskMetadata>(
        repo: &dyn TaskRepository,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let task = Self::build_pending_task(metadata, run_at);
        repo.create(&task).await
    }
}
