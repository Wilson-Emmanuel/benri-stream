//! Ambient trace-id context propagated via a tokio task-local.
//!
//! The value is read by [`crate::task::scheduler::TaskScheduler::build_pending_task`]
//! so that any task created inside a `with_trace_id(...)` scope inherits
//! the trace id of the caller — no plumbing required at every call site.
//!
//! Typical wiring:
//!
//! - **api**: a middleware generates (or extracts from `X-Request-Id` /
//!   `traceparent`) a trace id per incoming request, records it on the
//!   current tracing span so it shows up in request logs, and runs the
//!   downstream handler inside `with_trace_id(Some(id), next).await`.
//!   Use cases invoked by that handler call into `TaskScheduler`, which
//!   calls `current_trace_id()` and stamps the id on the new row.
//!
//! - **worker**: before the consumer dispatches a task to its handler,
//!   it wraps the dispatch in `with_trace_id(task.trace_id.clone(), …)`
//!   so that any *sub-tasks* the handler schedules inherit the same
//!   trace id as the task that triggered them. The task's own
//!   `trace_id` field is also recorded on the `task_handler` span so
//!   handler logs carry it.
//!
//! Outside of any scope, `current_trace_id()` returns `None`, and the
//! scheduler stores `None` — identical to the pre-existing behavior, so
//! introducing this module is backward-compatible for tests that don't
//! opt in.

use std::future::Future;

tokio::task_local! {
    static TRACE_ID: Option<String>;
}

/// Return the trace id for the current async task, if a scope set one.
///
/// Returns `None` both when there is no active scope and when the
/// active scope explicitly set `None` — the scheduler treats both the
/// same way (no trace id on the row), so the distinction doesn't
/// matter here.
pub fn current_trace_id() -> Option<String> {
    TRACE_ID.try_with(|id| id.clone()).ok().flatten()
}

/// Run `f` with `trace_id` set as the ambient trace context. Any code
/// `.await`ed inside `f` observes `trace_id` via [`current_trace_id`].
///
/// The scope ends when `f` resolves; reads after the returned future
/// completes see whatever (if any) outer scope was active before.
pub async fn with_trace_id<F>(trace_id: Option<String>, f: F) -> F::Output
where
    F: Future,
{
    TRACE_ID.scope(trace_id, f).await
}
