mod common;

use axum::http::StatusCode;
use chrono::Utc;
use uuid::Uuid;

use common::{build_test_app, get, CDN_BASE_URL};
use domain::ports::video::VideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};
use infrastructure::postgres::video_repository::PostgresVideoRepository;

fn seed(status: VideoStatus) -> Video {
    Video {
        id: VideoId::new(),
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: format!("uploads/{}/original.mp4", VideoId::new().0),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn get_status_returns_current_status_for_existing_video() {
    let app = build_test_app().await;
    let repo = PostgresVideoRepository::new(app.pool.clone());
    let video = seed(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();

    let (status, body) = app
        .send(get(&format!("/api/videos/{}/status", video.id.0)))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "PENDING_UPLOAD");
    assert!(body["share_url"].is_null());
}

#[tokio::test]
async fn get_status_returns_404_for_missing_video() {
    let app = build_test_app().await;
    let (status, body) = app
        .send(get(&format!("/api/videos/{}/status", Uuid::new_v4())))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "VIDEO_NOT_FOUND");
}

#[tokio::test]
async fn get_status_invalid_uuid_returns_400() {
    let app = build_test_app().await;
    let (status, body) = app.send(get("/api/videos/not-a-uuid/status")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_VIDEO_ID");
}

#[tokio::test]
async fn get_by_token_returns_title_and_stream_url_for_processed() {
    let app = build_test_app().await;
    let repo = PostgresVideoRepository::new(app.pool.clone());

    // Drive the video through PendingUpload → Uploaded → Processing →
    // Processed with a short share token.
    let video = seed(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();
    repo.update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    repo.update_status_if(&video.id, VideoStatus::Uploaded, VideoStatus::Processing)
        .await
        .unwrap();
    let token: String = VideoId::new().0.simple().to_string().chars().take(21).collect();
    repo.mark_processed(&video.id, &token).await.unwrap();

    let (status, body) = app
        .send(get(&format!("/api/videos/share/{token}")))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "t");
    let stream_url = body["stream_url"].as_str().unwrap();
    assert!(stream_url.starts_with(CDN_BASE_URL));
    assert!(stream_url.ends_with("/master.m3u8"));
}

#[tokio::test]
async fn get_by_token_returns_404_for_missing_token() {
    let app = build_test_app().await;
    let (status, body) = app.send(get("/api/videos/share/nope-nope-nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "VIDEO_NOT_FOUND");
}

#[tokio::test]
async fn get_by_token_returns_null_stream_url_when_not_processed() {
    // Short-circuit: insert a video with a share_token but status still
    // transient. The response should carry the title and a null
    // stream_url — confirms the use case honors is_streamable().
    let app = build_test_app().await;

    let mut video = seed(VideoStatus::PendingUpload);
    let token: String = VideoId::new().0.simple().to_string().chars().take(21).collect();
    video.share_token = Some(token.clone());
    // We can't insert directly with share_token + PendingUpload because
    // the production path only assigns tokens on Processed. Use a raw
    // SQL insert here — this test is deliberately checking a shape
    // invariant, not a reachable state.
    sqlx::query(
        "INSERT INTO videos (id, share_token, title, format, status, upload_key, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(video.id.0)
    .bind(&token)
    .bind(&video.title)
    .bind(video.format.as_str())
    .bind(video.status.as_str())
    .bind(&video.upload_key)
    .bind(video.created_at)
    .execute(&app.pool)
    .await
    .unwrap();

    let (status, body) = app
        .send(get(&format!("/api/videos/share/{token}")))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["stream_url"].is_null());
}
