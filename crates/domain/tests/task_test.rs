use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use domain::task::result::{OutcomeKind, TaskResult};
use domain::task::{Task, TaskId, TaskMetadata, TaskStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoRetryMeta;
impl TaskMetadata for NoRetryMeta {
    fn metadata_type_name(&self) -> &'static str {
        "NoRetryMeta"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetryableMeta {
    max: i32,
}
impl TaskMetadata for RetryableMeta {
    fn metadata_type_name(&self) -> &'static str {
        "RetryableMeta"
    }
    fn max_retries(&self) -> Option<i32> {
        Some(self.max)
    }
    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(10)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecurringMeta {
    interval_secs: u64,
}
impl TaskMetadata for RecurringMeta {
    fn metadata_type_name(&self) -> &'static str {
        "RecurringMeta"
    }
    fn execution_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(self.interval_secs))
    }
}

fn make_task(attempt_count: i32) -> Task {
    let now = Utc::now();
    Task {
        id: TaskId::new(),
        metadata_type: "Test".into(),
        metadata: serde_json::Value::Null,
        status: TaskStatus::InProgress,
        ordering_key: None,
        trace_id: None,
        attempt_count,
        next_run_at: now,
        error: None,
        started_at: Some(now),
        completed_at: None,
        created_at: now,
        updated_at: now,
    }
}

// ---- TaskStatus::from_str ----

#[test]
fn task_status_from_str_round_trip() {
    for status in [
        TaskStatus::Pending,
        TaskStatus::InProgress,
        TaskStatus::Completed,
        TaskStatus::DeadLetter,
    ] {
        assert_eq!(TaskStatus::from_str(status.as_str()), Some(status));
    }
}

#[test]
fn task_status_from_str_unknown_returns_none() {
    assert_eq!(TaskStatus::from_str("NOPE"), None);
    assert_eq!(TaskStatus::from_str(""), None);
}

// ---- compute_update: Success (one-shot) ----

#[test]
fn success_one_shot_transitions_to_completed() {
    let task = make_task(2);
    let result = TaskResult::Success {
        message: None,
        reschedule_after: None,
    };
    let outcome = task.compute_update(&NoRetryMeta, &result);
    assert_eq!(outcome.update.status, TaskStatus::Completed);
    assert_eq!(outcome.update.attempt_count, 2);
    assert!(outcome.update.next_run_at.is_none());
    assert!(outcome.update.completed_at.is_some());
    assert!(outcome.update.error.is_none());
    assert_eq!(outcome.kind, OutcomeKind::Success);
}

// ---- compute_update: Success (recurring) ----

#[test]
fn success_recurring_reschedules_and_resets_attempts() {
    let task = make_task(5);
    let result = TaskResult::Success {
        message: None,
        reschedule_after: None,
    };
    let outcome = task.compute_update(&RecurringMeta { interval_secs: 60 }, &result);
    assert_eq!(outcome.update.status, TaskStatus::Pending);
    assert_eq!(outcome.update.attempt_count, 0);
    assert!(outcome.update.next_run_at.is_some());
    assert_eq!(outcome.kind, OutcomeKind::Success);
}

#[test]
fn success_reschedule_after_overrides_interval() {
    let task = make_task(0);
    let result = TaskResult::Success {
        message: None,
        reschedule_after: Some(Duration::from_secs(5)),
    };
    let outcome = task.compute_update(&RecurringMeta { interval_secs: 3600 }, &result);
    // next_run_at should be ~5s from now, not 3600s
    let delay = outcome
        .update
        .next_run_at
        .unwrap()
        .signed_duration_since(Utc::now())
        .num_seconds();
    assert!(delay < 10, "expected ~5s, got {delay}s");
}

// ---- compute_update: RetryableFailure ----

#[test]
fn retryable_failure_without_retries_configured_goes_to_dead_letter() {
    let task = make_task(0);
    let result = TaskResult::RetryableFailure {
        error: "boom".into(),
        retry_after: None,
    };
    let outcome = task.compute_update(&NoRetryMeta, &result);
    assert_eq!(outcome.update.status, TaskStatus::DeadLetter);
    assert_eq!(outcome.update.attempt_count, 1);
    assert_eq!(outcome.update.error.as_deref(), Some("boom"));
    assert_eq!(outcome.kind, OutcomeKind::Failed);
}

#[test]
fn retryable_failure_with_retries_remaining_reschedules() {
    let task = make_task(2);
    let result = TaskResult::RetryableFailure {
        error: "transient".into(),
        retry_after: None,
    };
    let outcome = task.compute_update(&RetryableMeta { max: 5 }, &result);
    assert_eq!(outcome.update.status, TaskStatus::Pending);
    assert_eq!(outcome.update.attempt_count, 3);
    assert!(outcome.update.next_run_at.is_some());
    assert_eq!(outcome.update.error.as_deref(), Some("transient"));
    assert_eq!(outcome.kind, OutcomeKind::Retried);
}

#[test]
fn retryable_failure_at_max_retries_goes_to_dead_letter() {
    // attempt_count == max means all retries used
    let task = make_task(3);
    let result = TaskResult::RetryableFailure {
        error: "final".into(),
        retry_after: None,
    };
    let outcome = task.compute_update(&RetryableMeta { max: 3 }, &result);
    assert_eq!(outcome.update.status, TaskStatus::DeadLetter);
    assert_eq!(outcome.update.attempt_count, 4);
    assert_eq!(outcome.kind, OutcomeKind::Failed);
}

#[test]
fn retry_after_overrides_calculated_backoff() {
    let task = make_task(0);
    let result = TaskResult::RetryableFailure {
        error: "slow down".into(),
        retry_after: Some(Duration::from_secs(2)),
    };
    let outcome = task.compute_update(&RetryableMeta { max: 5 }, &result);
    let delay = outcome
        .update
        .next_run_at
        .unwrap()
        .signed_duration_since(Utc::now())
        .num_seconds();
    assert!(delay < 5, "expected ~2s, got {delay}s");
}

#[test]
fn retry_backoff_doubles_per_attempt() {
    // Indirectly verify exponential backoff via compute_update.
    // base = 10s. attempt 0 → 10s, attempt 1 → 20s, attempt 2 → 40s.
    let meta = RetryableMeta { max: 10 };
    let result = TaskResult::RetryableFailure {
        error: "x".into(),
        retry_after: None,
    };
    for (attempt, expected_secs) in [(0i32, 10i64), (1, 20), (2, 40)] {
        let task = make_task(attempt);
        let outcome = task.compute_update(&meta, &result);
        let delay = outcome
            .update
            .next_run_at
            .unwrap()
            .signed_duration_since(Utc::now())
            .num_seconds();
        // Allow a small slack around the expected value.
        assert!(
            (delay - expected_secs).abs() <= 2,
            "attempt {attempt}: expected ~{expected_secs}s, got {delay}s"
        );
    }
}

#[test]
fn retry_backoff_capped_at_30_minutes() {
    // Large attempt counts cap at 30 min (1800s).
    let meta = RetryableMeta { max: i32::MAX };
    let result = TaskResult::RetryableFailure {
        error: "x".into(),
        retry_after: None,
    };
    let task = make_task(i32::MAX - 1);
    let outcome = task.compute_update(&meta, &result);
    let delay = outcome
        .update
        .next_run_at
        .unwrap()
        .signed_duration_since(Utc::now())
        .num_seconds();
    assert!((30 * 60 - 2..=30 * 60 + 2).contains(&delay), "got {delay}s");
}

// ---- compute_update: PermanentFailure ----

#[test]
fn permanent_failure_goes_to_dead_letter() {
    let task = make_task(1);
    let result = TaskResult::PermanentFailure {
        error: "bad input".into(),
    };
    let outcome = task.compute_update(&RetryableMeta { max: 5 }, &result);
    // Even with retries configured, PermanentFailure skips retry path
    assert_eq!(outcome.update.status, TaskStatus::DeadLetter);
    assert_eq!(outcome.update.attempt_count, 1);
    assert_eq!(outcome.update.error.as_deref(), Some("bad input"));
    assert_eq!(outcome.kind, OutcomeKind::Failed);
}

// ---- compute_update: Skip ----

#[test]
fn skip_one_shot_marks_completed_with_reason() {
    let task = make_task(0);
    let result = TaskResult::Skip {
        reason: "nothing to do".into(),
    };
    let outcome = task.compute_update(&NoRetryMeta, &result);
    assert_eq!(outcome.update.status, TaskStatus::Completed);
    assert_eq!(
        outcome.update.error.as_deref(),
        Some("Skipped: nothing to do")
    );
    assert_eq!(outcome.kind, OutcomeKind::Success);
}

#[test]
fn skip_recurring_reschedules() {
    let task = make_task(0);
    let result = TaskResult::Skip {
        reason: "not yet".into(),
    };
    let outcome = task.compute_update(&RecurringMeta { interval_secs: 120 }, &result);
    assert_eq!(outcome.update.status, TaskStatus::Pending);
    assert!(outcome.update.next_run_at.is_some());
    assert_eq!(outcome.update.error.as_deref(), Some("Skipped: not yet"));
    assert_eq!(outcome.kind, OutcomeKind::Success);
}

// ---- compute_update: Terminate ----

#[test]
fn terminate_marks_completed_even_on_recurring_task() {
    let task = make_task(0);
    let result = TaskResult::Terminate {
        reason: "done forever".into(),
    };
    let outcome = task.compute_update(&RecurringMeta { interval_secs: 60 }, &result);
    // Terminate wins over recurring reschedule
    assert_eq!(outcome.update.status, TaskStatus::Completed);
    assert!(outcome.update.next_run_at.is_none());
    assert_eq!(
        outcome.update.error.as_deref(),
        Some("Terminated: done forever")
    );
    assert_eq!(outcome.kind, OutcomeKind::Success);
}
