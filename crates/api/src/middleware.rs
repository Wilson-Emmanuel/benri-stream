//! Per-request trace id resolution and propagation.
//!
//! Reads `X-Request-Id` or `traceparent` from the incoming headers, falling
//! back to a freshly minted UUID. The resolved id is placed in request
//! extensions (for `TraceLayer`'s span) and in the ambient `with_trace_id`
//! scope so any tasks scheduled during the request inherit it.

use axum::{
    extract::Request,
    http::{header::HeaderMap, HeaderName},
    middleware::Next,
    response::Response,
};

use domain::task::trace_context::with_trace_id;

/// Resolved trace id for the current request, stashed in extensions for
/// `TraceLayer`'s `make_span_with`.
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");

fn resolve_trace_id(headers: &HeaderMap) -> String {
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

/// Resolves the trace id, stashes it in request extensions, and wraps the
/// handler in a `with_trace_id` scope so scheduler calls inherit it.
pub async fn trace_id_middleware(mut req: Request, next: Next) -> Response {
    let trace_id = resolve_trace_id(req.headers());
    req.extensions_mut().insert(TraceId(trace_id.clone()));
    with_trace_id(Some(trace_id), next.run(req)).await
}
