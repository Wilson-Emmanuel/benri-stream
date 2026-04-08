mod common;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use common::{build_test_app, json_post};
use domain::ports::video::VideoRepository;
use domain::video::{VideoId, VideoStatus};
use infrastructure::postgres::video_repository::PostgresVideoRepository;

#[tokio::test]
async fn happy_path_persists_video_and_returns_upload_url() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post(
            "/api/videos/initiate",
            json!({ "title": "My Clip", "mime_type": "video/mp4" }),
        ))
        .await;

    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    assert!(body["upload_url"].as_str().unwrap().starts_with("http://"));

    let repo = PostgresVideoRepository::new(app.pool.clone());
    let video = repo.find_by_id(&VideoId(id)).await.unwrap().unwrap();
    assert_eq!(video.title, "My Clip");
    assert_eq!(video.status, VideoStatus::PendingUpload);
}

#[tokio::test]
async fn empty_title_returns_400_title_required() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post(
            "/api/videos/initiate",
            json!({ "title": "   ", "mime_type": "video/mp4" }),
        ))
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "TITLE_REQUIRED");
}

#[tokio::test]
async fn title_too_long_returns_400_title_too_long() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post(
            "/api/videos/initiate",
            json!({ "title": "a".repeat(101), "mime_type": "video/mp4" }),
        ))
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "TITLE_TOO_LONG");
}

#[tokio::test]
async fn unsupported_mime_type_returns_400_unsupported_format() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(json_post(
            "/api/videos/initiate",
            json!({ "title": "x", "mime_type": "image/png" }),
        ))
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "UNSUPPORTED_FORMAT");
}

#[tokio::test]
async fn malformed_json_returns_400() {
    let app = build_test_app().await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/videos/initiate")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let (status, _body) = app.send(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
