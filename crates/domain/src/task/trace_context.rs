//! Ambient trace-id propagated via a tokio task-local.
//!
//! `TaskScheduler::build_pending_task` reads this so tasks created inside
//! a `with_trace_id` scope inherit the caller's trace id without any
//! per-call-site plumbing.
//!
//! The api layer sets the scope per request; the worker wraps each dispatch
//! so sub-tasks inherit the same id as the task that triggered them.

use std::future::Future;

tokio::task_local! {
    static TRACE_ID: Option<String>;
}

/// Returns the trace id set by the innermost enclosing `with_trace_id` scope,
/// or `None` if no scope is active or the scope was set to `None`.
pub fn current_trace_id() -> Option<String> {
    TRACE_ID.try_with(|id| id.clone()).ok().flatten()
}

/// Runs `f` with `trace_id` as the ambient trace context for the duration
/// of the future. Code inside `f` reads it via [`current_trace_id`].
pub async fn with_trace_id<F>(trace_id: Option<String>, f: F) -> F::Output
where
    F: Future,
{
    TRACE_ID.scope(trace_id, f).await
}
