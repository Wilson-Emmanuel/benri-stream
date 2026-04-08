use chrono::Utc;
use serde::{Deserialize, Serialize};

use domain::task::scheduler::TaskScheduler;
use domain::task::{TaskMetadata, TaskStatus};

#[derive(Debug, Serialize, Deserialize)]
struct DummyMeta {
    video_id: String,
}

impl TaskMetadata for DummyMeta {
    fn metadata_type_name(&self) -> &'static str {
        "DummyMeta"
    }
    fn ordering_key(&self) -> Option<String> {
        Some(format!("video:{}", self.video_id))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BareMeta;

impl TaskMetadata for BareMeta {
    fn metadata_type_name(&self) -> &'static str {
        "BareMeta"
    }
}

#[test]
fn build_pending_task_uses_defaults_when_no_run_at() {
    let before = Utc::now();
    let task = TaskScheduler::build_pending_task(&BareMeta, None).unwrap();
    let after = Utc::now();

    assert_eq!(task.status, TaskStatus::Pending);
    assert_eq!(task.attempt_count, 0);
    assert_eq!(task.metadata_type, "BareMeta");
    assert!(task.error.is_none());
    assert!(task.started_at.is_none());
    assert!(task.completed_at.is_none());
    assert!(task.trace_id.is_none());
    assert!(task.ordering_key.is_none());
    assert!(task.next_run_at >= before && task.next_run_at <= after);
    assert!(task.created_at >= before && task.created_at <= after);
    assert_eq!(task.created_at, task.updated_at);
}

#[test]
fn build_pending_task_serializes_metadata_to_json() {
    let task = TaskScheduler::build_pending_task(
        &DummyMeta {
            video_id: "abc".into(),
        },
        None,
    )
    .unwrap();

    assert_eq!(task.metadata["video_id"], "abc");
    assert_eq!(task.metadata_type, "DummyMeta");
}

#[test]
fn build_pending_task_propagates_ordering_key() {
    let task = TaskScheduler::build_pending_task(
        &DummyMeta {
            video_id: "xyz".into(),
        },
        None,
    )
    .unwrap();

    assert_eq!(task.ordering_key.as_deref(), Some("video:xyz"));
}

#[test]
fn build_pending_task_uses_explicit_run_at() {
    let when = Utc::now() + chrono::Duration::hours(2);
    let task = TaskScheduler::build_pending_task(&BareMeta, Some(when)).unwrap();
    assert_eq!(task.next_run_at, when);
}
