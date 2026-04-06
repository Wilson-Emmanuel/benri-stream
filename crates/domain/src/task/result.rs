use std::time::Duration;

/// Result of task execution returned by task handlers. Controls the state
/// transition computed by `Task::compute_update`.
#[derive(Debug, Clone)]
pub enum TaskResult {
    /// Task completed successfully.
    ///
    /// For recurring tasks (metadata.execution_interval is set), this
    /// schedules the next execution. `reschedule_after` overrides the
    /// configured interval on a per-invocation basis.
    Success {
        message: Option<String>,
        /// Override the calculated next execution time for recurring tasks.
        reschedule_after: Option<Duration>,
    },

    /// Task failed with a retryable error.
    ///
    /// Retries according to the metadata's `max_retries` and
    /// `retry_base_delay`. Moves to `DEAD_LETTER` once retries are
    /// exhausted. `retry_after` overrides the calculated backoff.
    RetryableFailure {
        error: String,
        /// Override the calculated retry delay.
        retry_after: Option<Duration>,
    },

    /// Task failed with a permanent error. Moves to `DEAD_LETTER` immediately.
    PermanentFailure { error: String },

    /// Skip — preconditions not met.
    ///
    /// For one-shot tasks, marks as `COMPLETED`. For recurring tasks,
    /// reschedules without counting as a failure.
    Skip { reason: String },

    /// Terminate a recurring task.
    ///
    /// Marks the task `COMPLETED` and prevents future executions. Use this
    /// when a recurring task's work is done and should not run again until
    /// the system task checker recreates it (if applicable).
    Terminate { reason: String },
}
