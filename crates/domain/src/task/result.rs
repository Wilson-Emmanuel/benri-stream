use std::time::Duration;

/// What `Task::compute_update` decided about a task run, summarized for
/// metric labeling. Derived directly from the original `TaskResult`
/// variant plus retry state — **not** from `TaskUpdate.status`, which
/// is ambiguous (`Pending` can mean either a successful recurring
/// reschedule OR a retry-after-failure).
///
/// Lives next to `TaskResult` because the two are computed together
/// in one place (`compute_update`) and the `OutcomeKind` is the
/// caller-facing summary of which variant was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// Handler completed successfully (one-shot or recurring), or chose
    /// to skip on this run, or terminated a recurring task. None of
    /// these are failures.
    Success,
    /// Handler returned `RetryableFailure` and retries remain — the
    /// task will be re-attempted.
    Retried,
    /// Permanent failure: `PermanentFailure`, retries exhausted, bad
    /// metadata, or no handler registered.
    Failed,
}

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
