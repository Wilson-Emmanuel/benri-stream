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

impl Task {
    pub fn compute_update(&self, result: &result::TaskResult) -> TaskUpdate {
        let now = Utc::now();
        match result {
            result::TaskResult::Success { .. } => TaskUpdate {
                task_id: self.id.clone(),
                status: TaskStatus::Completed,
                attempt_count: self.attempt_count,
                next_run_at: None,
                error: None,
                completed_at: Some(now),
                updated_at: now,
            },
            result::TaskResult::RetryableFailure { error } => {
                TaskUpdate {
                    task_id: self.id.clone(),
                    status: TaskStatus::Pending,
                    attempt_count: self.attempt_count + 1,
                    next_run_at: Some(now + chrono::Duration::seconds(30 * (1 << self.attempt_count.min(10) as i64))),
                    error: Some(error.clone()),
                    completed_at: None,
                    updated_at: now,
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
            result::TaskResult::Skip { .. } => TaskUpdate {
                task_id: self.id.clone(),
                status: TaskStatus::Completed,
                attempt_count: self.attempt_count,
                next_run_at: None,
                error: None,
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

pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(300)
    }
    fn execution_interval(&self) -> Option<Duration> {
        None
    }
    fn max_retries(&self) -> Option<i32> {
        None
    }
    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(30)
    }
    fn ordering_key(&self) -> Option<String> {
        None
    }
    fn is_system_task(&self) -> bool {
        false
    }
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
