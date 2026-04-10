//! Library surface for the api crate. `main.rs` is a thin entry point
//! that builds an `AppState` from real infrastructure clients and hands
//! it to [`build_router`]; integration tests build their own
//! `AppState` from mocked or containerized dependencies and call
//! `build_router` directly.

pub mod handlers;
pub mod middleware;

use std::sync::Arc;

use axum::{
    extract::Request,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use application::usecases::video::{
    complete_upload::CompleteUploadUseCase, get_video_by_token::GetVideoByTokenUseCase,
    get_video_status::GetVideoStatusUseCase, initiate_upload::InitiateUploadUseCase,
};

use crate::middleware::{trace_id_middleware, TraceId};

#[derive(Clone)]
pub struct AppState {
    pub initiate_upload: Arc<InitiateUploadUseCase>,
    pub complete_upload: Arc<CompleteUploadUseCase>,
    pub get_video_status: Arc<GetVideoStatusUseCase>,
    pub get_video_by_token: Arc<GetVideoByTokenUseCase>,
}

/// Construct the Axum router with all routes, middleware, and the
/// composed use-case state. Shared by `main.rs` and integration tests.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/videos/initiate", post(handlers::video::initiate_upload))
        .route(
            "/api/videos/{id}/complete",
            post(handlers::video::complete_upload),
        )
        .route(
            "/api/videos/{id}/status",
            get(handlers::video::get_video_status),
        )
        .route(
            "/api/videos/share/{share_token}",
            get(handlers::video::get_video_by_token),
        )
        .route("/health", get(|| async { "ok" }))
        // Order matters: layers applied later are outer. We want
        // `trace_id_middleware` to run *before* `TraceLayer` so it can
        // stash the resolved id in request extensions for
        // `TraceLayer`'s `make_span_with` to read. Tower applies layers
        // bottom-up: the last `.layer()` in source order becomes the
        // outermost wrapping, so `from_fn(trace_id_middleware)` below
        // wraps `TraceLayer` above it, and its pre-handler code runs
        // first.
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &Request| {
                // Read the trace id our middleware stashed. If it's
                // missing (shouldn't happen in normal flow), fall back
                // to the empty string so the span field is still
                // present in the JSON output — easier to grep for
                // "missing trace id" than a silently absent key.
                let trace_id = req
                    .extensions()
                    .get::<TraceId>()
                    .map(|t| t.0.as_str())
                    .unwrap_or("");
                tracing::info_span!(
                    "request",
                    method = %req.method(),
                    uri = %req.uri(),
                    version = ?req.version(),
                    trace_id = trace_id,
                )
            }),
        )
        .layer(axum::middleware::from_fn(trace_id_middleware))
        .layer(CorsLayer::permissive())
        .with_state(state)
}
