use chrono::{DateTime, Utc};

use crate::ports::unit_of_work::TaskMutations;
use crate::ports::video::RepositoryError;
use super::{Task, TaskId, TaskMetadata, TaskStatus};

/// Stateless entry point for creating tasks. Use cases call this inside a
/// `TxScope` — see `crate::ports::unit_of_work`. Never call `TaskMutations::create`
/// directly from a use case; always go through `TaskScheduler::schedule`.
pub struct TaskScheduler;

impl TaskScheduler {
    /// Schedules a task inside the caller's transaction.
    ///
    /// When the metadata declares an `ordering_key`, this is dedup-by-default:
    /// if an active (`PENDING` or `IN_PROGRESS`) task already exists for the
    /// same `(metadata_type, ordering_key)` pair, the existing task is
    /// returned instead of creating a duplicate. The dedup check runs in the
    /// same transaction as the subsequent insert — and the database backs it
    /// up with a partial unique index in case a race slips through.
    ///
    /// Scheduling config (`max_retries`, `retry_base_delay`,
    /// `execution_interval`, `processing_timeout`) is read from the metadata
    /// trait and persisted into the `tasks` row so the consumer can honor it
    /// without reconstructing the concrete type.
    ///
    /// `run_at` lets callers schedule a task for a future time. Defaults to
    /// `now` when `None`.
    pub async fn schedule<M: TaskMetadata>(
        tasks: &mut dyn TaskMutations,
        metadata: &M,
        trace_id: Option<String>,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        let metadata_type = metadata.metadata_type_name();

        // Dedup-by-default when the metadata declares an ordering key.
        if let Some(ordering_key) = metadata.ordering_key() {
            if let Some(existing) = tasks
                .find_active_by_ordering_key(metadata_type, &ordering_key)
                .await?
            {
                return Ok(existing);
            }
        }

        let now = Utc::now();
        let metadata_json = serde_json::to_value(metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        let task = Task {
            id: TaskId::new(),
            metadata_type: metadata_type.to_string(),
            metadata: metadata_json,
            status: TaskStatus::Pending,
            ordering_key: metadata.ordering_key(),
            trace_id,
            attempt_count: 0,
            next_run_at: run_at.unwrap_or(now),
            error: None,
            started_at: None,
            completed_at: None,
            max_retries: metadata.max_retries(),
            retry_base_delay_ms: metadata.retry_base_delay().as_millis() as i64,
            execution_interval_ms: metadata
                .execution_interval()
                .map(|d| d.as_millis() as i64),
            processing_timeout_ms: metadata.processing_timeout().as_millis() as i64,
            created_at: now,
            updated_at: now,
        };

        tasks.create(&task).await
    }
}
