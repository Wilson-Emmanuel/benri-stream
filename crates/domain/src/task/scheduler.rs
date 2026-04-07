use chrono::{DateTime, Utc};

use crate::ports::unit_of_work::TaskMutations;
use crate::ports::video::RepositoryError;
use super::{Task, TaskId, TaskMetadata, TaskStatus};

/// Stateless entry point for creating tasks. Use cases call this inside a
/// `TxScope` opened via `UnitOfWork::begin` — see `crate::ports::unit_of_work`.
/// Never call `TaskMutations::create` directly from a use case; always go
/// through `TaskScheduler::schedule`.
///
/// **No deduplication.** Multiple `schedule()` calls for the same logical
/// task may create multiple rows. Handlers MUST be idempotent (see
/// `task-system.md` "Handler Idempotency"). For resource serialization
/// (preventing two handlers from running concurrently on the same
/// resource), use `TaskMetadata::ordering_key`.
pub struct TaskScheduler;

impl TaskScheduler {
    /// Schedules a task inside the caller's transaction.
    ///
    /// `run_at` lets callers schedule a task for a future time. Defaults to
    /// `now` when `None`.
    ///
    /// **trace_id**: not currently propagated. The Task is created with
    /// `trace_id: None`. When OpenTelemetry / a tracing-context port is
    /// wired, populate it here from the current span. Until then, the
    /// `trace_id` column on the row stays NULL and worker logs are not
    /// linked back to the originating request.
    pub async fn schedule<M: TaskMetadata>(
        tasks: &mut dyn TaskMutations,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let now = Utc::now();
        let metadata_json = serde_json::to_value(metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        let task = Task {
            id: TaskId::new(),
            metadata_type: metadata.metadata_type_name().to_string(),
            metadata: metadata_json,
            status: TaskStatus::Pending,
            ordering_key: metadata.ordering_key(),
            // TODO: populate from current trace context once OTel is wired.
            trace_id: None,
            attempt_count: 0,
            next_run_at: run_at.unwrap_or(now),
            error: None,
            started_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };

        tasks.create(&task).await
    }
}
