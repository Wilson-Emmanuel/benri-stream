//! Axum middleware for per-request trace id generation and propagation.
//!
//! The middleware has two jobs:
//!
//! 1. **Read or mint a trace id.** If the client sent `X-Request-Id` or
//!    the W3C `traceparent` header, use that; otherwise generate a
//!    fresh UUID. This means upstream callers (a reverse proxy, a
//!    tracing system, another service) can thread their own correlation
//!    id through without the api clobbering it, while direct browser
//!    hits still get something greppable.
//!
//! 2. **Propagate it two ways.** It goes into
//!    [`domain::task::trace_context`] via `with_trace_id` so the
//!    `TaskScheduler` can stamp it on any tasks use cases create while
//!    handling this request, *and* it goes into the request's
//!    extensions so the sibling `TraceLayer` can read it at span-
//!    creation time and include it as a field on the "request" span
//!    (so all tower-http access logs for the request carry the id).
//!
//! The layer ordering in `build_router` puts this middleware *inside*
//! the `TraceLayer` (applied after, so it's outer-wrapped by
//! `TraceLayer`'s call). That way `TraceLayer` opens its span first —
//! reading the trace id from extensions we inserted — and our
//! `with_trace_id` scope is active for the handler and anything it
//! awaits, including async use cases hitting the scheduler.

use axum::{
    extract::Request,
    http::{header::HeaderMap, HeaderName},
    middleware::Next,
    response::Response,
};

use domain::task::trace_context::with_trace_id;

/// Extension value holding the resolved trace id for the current
/// request. Picked up by `TraceLayer`'s `make_span_with` in
/// `build_router` to record the id on the request span.
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");

/// Resolve (or mint) the trace id for an incoming request.
fn resolve_trace_id(headers: &HeaderMap) -> String {
    // Prefer an explicit `X-Request-Id` from upstream — it's the
    // common proxy/edge convention and easier to set by hand in curl.
    // Fall back to `traceparent` so a real tracing system can thread
    // its id through. If neither is present, mint a UUID so we always
    // have something to correlate by.
    headers
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get(&TRACEPARENT)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string())
}

/// Axum middleware: resolves the trace id, stashes it in the request's
/// extensions (so `TraceLayer` can put it on the span), and runs the
/// inner handler inside a `with_trace_id` scope so downstream
/// scheduler calls stamp the id onto new task rows.
pub async fn trace_id_middleware(mut req: Request, next: Next) -> Response {
    let trace_id = resolve_trace_id(req.headers());
    req.extensions_mut().insert(TraceId(trace_id.clone()));
    with_trace_id(Some(trace_id), next.run(req)).await
}
