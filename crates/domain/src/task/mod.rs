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
/// **Scheduling config (max_retries, retry_base_delay, execution_interval,
/// processing_timeout) is NOT stored on the row.** It lives on the typed
/// `TaskMetadata` impl and is read at run time. The consumer's `HandlerAdapter`
/// deserializes the metadata before computing the update so the live trait
/// values are used. This means config changes apply immediately on the next
/// task run without a migration.
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const MAX_RETRY_BACKOFF_SECS: i64 = 30 * 60;
const MAX_BACKOFF_EXPONENT: i32 = 10;

impl Task {
    /// Pure function: given the typed metadata and a `TaskResult`, compute
    /// the `TaskUpdate` representing the next DB state. No side effects, no
    /// DB access.
    ///
    /// Generic over `M: TaskMetadata` because the per-task scheduling config
    /// (max retries, base delay, interval) lives on the trait, not on the row.
    /// The caller (typically `HandlerAdapter`) is responsible for
    /// deserializing the typed metadata from `task.metadata` and passing it.
    ///
    /// Note: `Skip` and `Terminate` outcomes write their reason into the
    /// `error` column prefixed with `"Skipped: "` / `"Terminated: "`. The
    /// `error` column doubles as a "last message" channel since there's no
    /// dedicated last-message column. Filter on the prefix when querying
    /// for actual failures.
    pub fn compute_update<M: TaskMetadata>(
        &self,
        metadata: &M,
        result: &result::TaskResult,
    ) -> TaskUpdate {
        let now = Utc::now();
        match result {
            result::TaskResult::Success { reschedule_after, .. } => {
                let next_run_at = reschedule_after
                    .map(|d| now + chrono::Duration::milliseconds(d.as_millis() as i64))
                    .or_else(|| {
                        metadata
                            .execution_interval()
                            .map(|d| now + chrono::Duration::milliseconds(d.as_millis() as i64))
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
                let can_retry = match metadata.max_retries() {
                    Some(max) => self.attempt_count < max,
                    None => false,
                };

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
                    let delay = retry_after
                        .map(|d| chrono::Duration::milliseconds(d.as_millis() as i64))
                        .unwrap_or_else(|| calculate_retry_delay(metadata, self.attempt_count));
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
                // Recurring tasks reschedule on skip; one-shot → Completed.
                if let Some(interval) = metadata.execution_interval() {
                    TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Pending,
                        attempt_count: self.attempt_count,
                        next_run_at: Some(
                            now + chrono::Duration::milliseconds(interval.as_millis() as i64),
                        ),
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

/// Exponential backoff: `base * 2^attempt_count`, capped at 30 minutes.
/// The exponent is clamped at 10 to prevent overflow.
fn calculate_retry_delay<M: TaskMetadata>(metadata: &M, attempt_count: i32) -> chrono::Duration {
    let base_ms = metadata.retry_base_delay().as_millis() as i64;
    let exp = attempt_count.min(MAX_BACKOFF_EXPONENT) as u32;
    let delay_ms = base_ms.saturating_mul(1i64 << exp);
    let capped_secs = (delay_ms / 1000).min(MAX_RETRY_BACKOFF_SECS);
    chrono::Duration::seconds(capped_secs.max(0))
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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "PENDING" => Some(Self::Pending),
            "IN_PROGRESS" => Some(Self::InProgress),
            "COMPLETED" => Some(Self::Completed),
            "DEAD_LETTER" => Some(Self::DeadLetter),
            _ => None,
        }
    }
}

/// Metadata for a specific task type. Implementations are data structs that
/// declare the scheduling config for instances of that type.
///
/// The `METADATA_TYPE` associated const (declared directly on the impl struct)
/// should equal the struct name and match the key used when registering the
/// handler in the dispatch map. `metadata_type_name()` returns that const.
///
/// **Processing timeout limit**: each task type's `processing_timeout()` MUST
/// be less than `STALE_RECOVERY_THRESHOLD - STALE_RECOVERY_BUFFER` so that
/// stale recovery never resets a task that's still legitimately running. The
/// current limit is 30 minutes (the global threshold is 1 hour).
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time the consumer will wait for the handler before cancelling.
    /// MUST be less than 30 minutes — see trait docstring.
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

    /// Tasks with the same ordering key are processed sequentially (no two
    /// active at once with the same key, enforced by `find_pending`'s CTE).
    /// Used for resource serialization, not deduplication — handlers must
    /// be idempotent because at-least-once delivery may invoke the same
    /// handler twice.
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
