pub mod metadata;
pub mod result;
pub mod scheduler;

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A scheduled or in-flight task.
///
/// Scheduling config (`max_retries`, `retry_base_delay_ms`,
/// `execution_interval_ms`, `processing_timeout_ms`) is denormalized from the
/// concrete `TaskMetadata` at schedule time and persisted as columns. This
/// lets the consumer and `compute_update` operate without reconstructing the
/// typed metadata from the JSON payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub metadata_type: String,
    pub metadata: serde_json::Value,
    pub status: TaskStatus,
    pub ordering_key: Option<String>,
    pub trace_id: Option<String>,
    pub attempt_count: i32,
    pub next_run_at: DateTime<Utc>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,

    // Denormalized scheduling config (see struct docstring).
    pub max_retries: Option<i32>,
    pub retry_base_delay_ms: i64,
    pub execution_interval_ms: Option<i64>,
    pub processing_timeout_ms: i64,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const MAX_RETRY_BACKOFF_SECS: i64 = 30 * 60;
const MAX_BACKOFF_EXPONENT: i32 = 10;

impl Task {
    pub fn processing_timeout(&self) -> Duration {
        Duration::from_millis(self.processing_timeout_ms.max(0) as u64)
    }

    pub fn is_recurring(&self) -> bool {
        self.execution_interval_ms.is_some()
    }

    pub fn can_retry(&self) -> bool {
        match self.max_retries {
            Some(max) => self.attempt_count < max,
            None => false,
        }
    }

    /// Exponential backoff: `retry_base_delay_ms * 2^attempt_count`, capped
    /// at 30 minutes. The exponent is clamped at 10 to prevent overflow.
    pub fn calculate_retry_delay(&self) -> chrono::Duration {
        let exp = self.attempt_count.min(MAX_BACKOFF_EXPONENT) as u32;
        let delay_ms = self
            .retry_base_delay_ms
            .saturating_mul(1i64 << exp);
        let capped_secs = (delay_ms / 1000).min(MAX_RETRY_BACKOFF_SECS);
        chrono::Duration::seconds(capped_secs.max(0))
    }

    /// Pure function: given a `TaskResult`, compute the `TaskUpdate`
    /// representing the next DB state. No side effects, no DB access.
    pub fn compute_update(&self, result: &result::TaskResult) -> TaskUpdate {
        let now = Utc::now();
        match result {
            result::TaskResult::Success { reschedule_after, .. } => {
                let next_run_at = reschedule_after
                    .map(|d| now + chrono::Duration::milliseconds(d.as_millis() as i64))
                    .or_else(|| {
                        self.execution_interval_ms
                            .map(|ms| now + chrono::Duration::milliseconds(ms))
                    });

                if let Some(next) = next_run_at {
                    // Recurring task → reschedule.
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Pending,
                        attempt_count: 0,
                        next_run_at: Some(next),
                        error: None,
                        completed_at: Some(now),
                        updated_at: now,
                    }
                } else {
                    // One-shot → completed.
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Completed,
                        attempt_count: self.attempt_count,
                        next_run_at: None,
                        error: None,
                        completed_at: Some(now),
                        updated_at: now,
                    }
                }
            }

            result::TaskResult::RetryableFailure { error, retry_after } => {
                let new_attempt = self.attempt_count + 1;
                // can_retry checks the PRE-increment attempt_count against
                // max_retries. If attempt_count < max_retries, the post-fail
                // retry is allowed; otherwise we've exhausted retries and
                // dead-letter.
                let can_retry = self.can_retry();

                if !can_retry {
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::DeadLetter,
                        attempt_count: new_attempt,
                        next_run_at: None,
                        error: Some(error.clone()),
                        completed_at: Some(now),
                        updated_at: now,
                    }
                } else {
                    // Backoff uses the PRE-increment attempt_count: first
                    // retry (from attempt_count = 0) delays by `base * 2^0 = base`.
                    let delay = retry_after
                        .map(|d| chrono::Duration::milliseconds(d.as_millis() as i64))
                        .unwrap_or_else(|| self.calculate_retry_delay());
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Pending,
                        attempt_count: new_attempt,
                        next_run_at: Some(now + delay),
                        error: Some(error.clone()),
                        completed_at: None,
                        updated_at: now,
                    }
                }
            }

            result::TaskResult::PermanentFailure { error } => TaskUpdate {
                task_id: self.id.clone(),
                status: TaskStatus::DeadLetter,
                attempt_count: self.attempt_count,
                next_run_at: None,
                error: Some(error.clone()),
                completed_at: Some(now),
                updated_at: now,
            },

            result::TaskResult::Skip { reason } => {
                // Recurring tasks reschedule on skip (preconditions not met,
                // try again next interval). One-shot tasks are marked completed.
                if let Some(ms) = self.execution_interval_ms {
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Pending,
                        attempt_count: self.attempt_count,
                        next_run_at: Some(now + chrono::Duration::milliseconds(ms)),
                        error: Some(format!("Skipped: {}", reason)),
                        completed_at: None,
                        updated_at: now,
                    }
                } else {
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Completed,
                        attempt_count: self.attempt_count,
                        next_run_at: None,
                        error: Some(format!("Skipped: {}", reason)),
                        completed_at: Some(now),
                        updated_at: now,
                    }
                }
            }

            result::TaskResult::Terminate { reason } => TaskUpdate {
                task_id: self.id.clone(),
                status: TaskStatus::Completed,
                attempt_count: self.attempt_count,
                next_run_at: None,
                error: Some(format!("Terminated: {}", reason)),
                completed_at: Some(now),
                updated_at: now,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    DeadLetter,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::InProgress => "IN_PROGRESS",
            Self::Completed => "COMPLETED",
            Self::DeadLetter => "DEAD_LETTER",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "PENDING" => Self::Pending,
            "IN_PROGRESS" => Self::InProgress,
            "COMPLETED" => Self::Completed,
            "DEAD_LETTER" => Self::DeadLetter,
            _ => Self::Pending,
        }
    }
}

/// Metadata for a specific task type. Implementations are data structs that
/// declare the scheduling config for instances of that type.
///
/// The `METADATA_TYPE` associated const (declared directly on the impl struct)
/// should equal the struct name and match the key used when registering the
/// handler in the dispatch map. `metadata_type_name()` returns that const.
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time the consumer will wait for the handler before cancelling.
    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(300)
    }

    /// For recurring tasks, the delay between successful runs. `None` for
    /// one-shot tasks.
    fn execution_interval(&self) -> Option<Duration> {
        None
    }

    /// Max retries on `RetryableFailure`. `None` = no retries (straight to
    /// dead letter on first failure).
    fn max_retries(&self) -> Option<i32> {
        None
    }

    /// Base for exponential backoff. Actual delay = `base * 2^attempt_count`,
    /// capped at 30 minutes.
    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Tasks with the same ordering key are processed sequentially and are
    /// dedup-by-default on schedule.
    fn ordering_key(&self) -> Option<String> {
        None
    }

    /// System tasks are recurring infrastructure tasks that the system task
    /// checker recreates if no active instance exists.
    fn is_system_task(&self) -> bool {
        false
    }

    /// Routing key used to match tasks to handlers. Must equal the struct's
    /// `METADATA_TYPE` const.
    fn metadata_type_name(&self) -> &'static str;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub attempt_count: i32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}
