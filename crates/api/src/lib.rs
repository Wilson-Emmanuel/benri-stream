//! Library surface for the api crate. `main.rs` is a thin entry point
//! that builds an `AppState` from real infrastructure clients and hands
//! it to [`build_router`]; integration tests build their own
//! `AppState` from mocked or containerized dependencies and call
//! `build_router` directly.

pub mod handlers;

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use application::usecases::video::{
    complete_upload::CompleteUploadUseCase, get_video_by_token::GetVideoByTokenUseCase,
    get_video_status::GetVideoStatusUseCase, initiate_upload::InitiateUploadUseCase,
};

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
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}
