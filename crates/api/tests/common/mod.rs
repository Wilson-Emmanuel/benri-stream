#![allow(dead_code)]
//! Integration-test harness for the api crate.
//!
//! Each test builds a fresh `Router` against a shared Postgres and
//! MinIO container (via `infrastructure::testing`). No mocks — the full
//! stack runs: HTTP → handler → use case → real repository → real DB /
//! S3. This is the level at which routing, status codes, and error
//! shape can only be verified end-to-end.
//!
//! Tests use unique `VideoId`s to stay independent inside a shared DB.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use application::usecases::video::{
    complete_upload::CompleteUploadUseCase, get_video_by_token::GetVideoByTokenUseCase,
    get_video_status::GetVideoStatusUseCase, initiate_upload::InitiateUploadUseCase,
};
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::postgres::transaction::PgTransactionPort;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::testing::{minio_client, minio_endpoint, pg_pool};

use api::{build_router, AppState};

pub const BASE_URL: &str = "http://test.local";
pub const CDN_BASE_URL: &str = "http://cdn.test";

/// Build a fresh `Router` wired to real Postgres + MinIO. Returns the
/// router plus the pool and S3 client so tests can seed data directly
/// without going through the HTTP surface.
pub async fn build_test_app() -> TestApp {
    let pool = pg_pool().await;
    let s3 = minio_client().await;
    let ep = minio_endpoint().await;

    // Tests point at a single MinIO container — no browser/backend
    // split needed, so no upload_presign_client override.
    let storage: Arc<dyn domain::ports::storage::StoragePort> = Arc::new(S3StorageClient::new(
        s3.clone(),
        ep.upload_bucket.clone(),
        ep.output_bucket.clone(),
        CDN_BASE_URL.into(),
    ));

    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn domain::ports::task::TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool.clone()));
    let tx_port: Arc<dyn domain::ports::transaction::TransactionPort> =
        Arc::new(PgTransactionPort::new(pool.clone()));

    let state = AppState {
        initiate_upload: Arc::new(InitiateUploadUseCase::new(
            video_repo.clone(),
            storage.clone(),
        )),
        complete_upload: Arc::new(CompleteUploadUseCase::new(
            video_repo.clone(),
            task_repo,
            tx_port,
            storage.clone(),
        )),
        get_video_status: Arc::new(GetVideoStatusUseCase::new(
            video_repo.clone(),
            BASE_URL.into(),
        )),
        get_video_by_token: Arc::new(GetVideoByTokenUseCase::new(
            video_repo.clone(),
            CDN_BASE_URL.into(),
        )),
    };

    TestApp {
        router: build_router(state),
        pool,
        s3,
        upload_bucket: ep.upload_bucket.clone(),
        _output_bucket: ep.output_bucket.clone(),
    }
}

pub struct TestApp {
    pub router: Router,
    pub pool: sqlx::PgPool,
    pub s3: aws_sdk_s3::Client,
    pub upload_bucket: String,
    pub _output_bucket: String,
}

impl TestApp {
    /// Send a request through the router and collect the response into
    /// a status + deserialized JSON body (or raw bytes for non-JSON
    /// routes).
    pub async fn send(&self, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp: Response<Body> = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("router oneshot");
        let status = resp.status();
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        let json: serde_json::Value = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }
}

/// Build a JSON POST request.
pub fn json_post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Build a GET request.
pub fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}
