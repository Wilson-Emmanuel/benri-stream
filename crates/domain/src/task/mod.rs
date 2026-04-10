pub mod metadata;
pub mod result;
pub mod scheduler;
pub mod trace_context;

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

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A scheduled or in-flight task.
///
/// Scheduling config (`max_retries`, `retry_base_delay`, `execution_interval`,
/// `processing_timeout`) is not stored on the row — it lives on the typed
/// `TaskMetadata` impl and is read at run time, so config changes take effect
/// immediately without a migration.
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

/// DB row update plus outcome classification for metric labeling,
/// produced together by `Task::compute_update`.
#[derive(Debug, Clone)]
pub struct TaskRunOutcome {
    pub update: TaskUpdate,
    pub kind: result::OutcomeKind,
}

impl Task {
    /// Computes the next DB state and outcome classification from a `TaskResult`.
    /// No side effects; the caller (typically `HandlerAdapter`) deserializes the
    /// typed metadata and passes it in.
    ///
    /// `Skip` and `Terminate` write their reason into the `error` column prefixed
    /// with `"Skipped: "` / `"Terminated: "` — filter on those prefixes when
    /// querying for actual failures.
    pub fn compute_update<M: TaskMetadata>(
        &self,
        metadata: &M,
        result: &result::TaskResult,
    ) -> TaskRunOutcome {
        use result::OutcomeKind;
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

                let update = if let Some(next) = next_run_at {
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
                };
                TaskRunOutcome { update, kind: OutcomeKind::Success }
            }

            result::TaskResult::RetryableFailure { error, retry_after } => {
                let new_attempt = self.attempt_count + 1;
                let can_retry = match metadata.max_retries() {
                    Some(max) => self.attempt_count < max,
                    None => false,
                };

                if !can_retry {
                    let update = TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::DeadLetter,
                        attempt_count: new_attempt,
                        next_run_at: None,
                        error: Some(error.clone()),
                        completed_at: Some(now),
                        updated_at: now,
                    };
                    TaskRunOutcome { update, kind: OutcomeKind::Failed }
                } else {
                    let delay = retry_after
                        .map(|d| chrono::Duration::milliseconds(d.as_millis() as i64))
                        .unwrap_or_else(|| calculate_retry_delay(metadata, self.attempt_count));
                    let update = TaskUpdate {
                        task_id: self.id.clone(),
                        status: TaskStatus::Pending,
                        attempt_count: new_attempt,
                        next_run_at: Some(now + delay),
                        error: Some(error.clone()),
                        completed_at: None,
                        updated_at: now,
                    };
                    TaskRunOutcome { update, kind: OutcomeKind::Retried }
                }
            }

            result::TaskResult::PermanentFailure { error } => {
                let update = TaskUpdate {
                    task_id: self.id.clone(),
                    status: TaskStatus::DeadLetter,
                    attempt_count: self.attempt_count,
                    next_run_at: None,
                    error: Some(error.clone()),
                    completed_at: Some(now),
                    updated_at: now,
                };
                TaskRunOutcome { update, kind: OutcomeKind::Failed }
            }

            result::TaskResult::Skip { reason } => {
                // Recurring tasks reschedule on skip; one-shot → Completed.
                let update = if let Some(interval) = metadata.execution_interval() {
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
                };
                TaskRunOutcome { update, kind: OutcomeKind::Success }
            }

            result::TaskResult::Terminate { reason } => {
                let update = TaskUpdate {
                    task_id: self.id.clone(),
                    status: TaskStatus::Completed,
                    attempt_count: self.attempt_count,
                    next_run_at: None,
                    error: Some(format!("Terminated: {}", reason)),
                    completed_at: Some(now),
                    updated_at: now,
                };
                TaskRunOutcome { update, kind: OutcomeKind::Success }
            }
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

    // Returns Option (not Result like std FromStr) because the only
    // caller is the row mapper, which panics on None — there is no
    // useful error value to wrap and propagate.
    #[allow(clippy::should_implement_trait)]
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

/// Scheduling configuration and routing for a task type.
///
/// Each implementation is a data struct. `metadata_type_name()` must return
/// the same string used to register the handler in the dispatch map.
///
/// `processing_timeout()` must be less than 30 minutes so stale-recovery
/// never resets a legitimately running task (the global stale threshold is
/// 1 hour).
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time the consumer waits for the handler before cancelling.
    /// Must be less than 30 minutes — see trait doc.
    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(300)
    }

    /// Delay between successful runs for recurring tasks. `None` for one-shot tasks.
    fn execution_interval(&self) -> Option<Duration> {
        None
    }

    /// Max retries on `RetryableFailure`. `None` means no retries.
    fn max_retries(&self) -> Option<i32> {
        None
    }

    /// Base for exponential backoff (`base * 2^attempt_count`, capped at 30 minutes).
    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Tasks with the same ordering key run sequentially — `find_pending`'s CTE
    /// skips keys already in-progress. This serializes concurrent access to a
    /// resource; it is not deduplication (handlers must still be idempotent).
    fn ordering_key(&self) -> Option<String> {
        None
    }

    /// Recurring infrastructure tasks that the system-task checker recreates
    /// when no active instance exists.
    fn is_system_task(&self) -> bool {
        false
    }

    /// Routing key used to match tasks to handlers.
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
