mod common;

use axum::http::StatusCode;
use aws_sdk_s3::primitives::ByteStream;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use common::{build_test_app, get, json_post, TestApp};
use domain::ports::video::VideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};
use infrastructure::postgres::video_repository::PostgresVideoRepository;

/// Insert a PendingUpload video row directly and put a body under its
/// upload key so complete_upload has something to inspect.
async fn seed_pending_upload(app: &TestApp, body: Vec<u8>) -> Video {
    let id = VideoId::new();
    let upload_key = format!("uploads/{}/original.mp4", id.0);
    let video = Video {
        id: id.clone(),
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::PendingUpload,
        upload_key: upload_key.clone(),
        created_at: Utc::now(),
    };
    PostgresVideoRepository::new(app.pool.clone())
        .insert(&video)
        .await
        .unwrap();

    app.s3
        .put_object()
        .bucket(&app.upload_bucket)
        .key(&upload_key)
        .content_type("video/mp4")
        .body(ByteStream::from(body))
        .send()
        .await
        .unwrap();

    video
}

fn mp4_body() -> Vec<u8> {
    // bytes[4..8] == "ftyp" is all validate_signature checks.
    let mut b = vec![0u8; 64];
    b[4..8].copy_from_slice(b"ftyp");
    b
}

#[tokio::test]
async fn happy_path_returns_uploaded_status() {
    let app = build_test_app().await;
    let video = seed_pending_upload(&app, mp4_body()).await;

    let (status, body) = app
        .send(json_post(
            &format!("/api/videos/{}/complete", video.id.0),
            json!({}),
        ))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UPLOADED");
    assert_eq!(body["id"], video.id.0.to_string());
}

#[tokio::test]
async fn missing_video_returns_404() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post(
            &format!("/api/videos/{}/complete", Uuid::new_v4()),
            json!({}),
        ))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "VIDEO_NOT_FOUND");
}

#[tokio::test]
async fn invalid_signature_returns_422() {
    let app = build_test_app().await;
    // All-zeros body: no 'ftyp' at offset 4.
    let video = seed_pending_upload(&app, vec![0u8; 64]).await;

    let (status, body) = app
        .send(json_post(
            &format!("/api/videos/{}/complete", video.id.0),
            json!({}),
        ))
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "INVALID_FILE_SIGNATURE");
}

#[tokio::test]
async fn malformed_uuid_returns_400() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post("/api/videos/not-a-uuid/complete", json!({})))
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_VIDEO_ID");
}

#[tokio::test]
async fn second_complete_returns_409_already_completed() {
    let app = build_test_app().await;
    let video = seed_pending_upload(&app, mp4_body()).await;

    // First call succeeds.
    let (first_status, _) = app
        .send(json_post(
            &format!("/api/videos/{}/complete", video.id.0),
            json!({}),
        ))
        .await;
    assert_eq!(first_status, StatusCode::OK);

    // Second call — status is now UPLOADED, not PENDING_UPLOAD.
    let (status, body) = app
        .send(json_post(
            &format!("/api/videos/{}/complete", video.id.0),
            json!({}),
        ))
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "ALREADY_COMPLETED");
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let app = build_test_app().await;
    let (status, _) = app.send(get("/health")).await;
    assert_eq!(status, StatusCode::OK);
}
