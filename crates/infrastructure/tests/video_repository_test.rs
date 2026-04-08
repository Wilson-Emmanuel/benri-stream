#![cfg(feature = "test-support")]

use chrono::{Duration as ChronoDuration, Utc};

use domain::ports::video::VideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::testing::pg_pool;

fn fresh_video(status: VideoStatus) -> Video {
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
async fn insert_then_find_by_id_round_trips() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let video = fresh_video(VideoStatus::PendingUpload);

    repo.insert(&video).await.unwrap();
    let got = repo.find_by_id(&video.id).await.unwrap().unwrap();

    assert_eq!(got.id, video.id);
    assert_eq!(got.title, video.title);
    assert_eq!(got.format, VideoFormat::Mp4);
    assert_eq!(got.status, VideoStatus::PendingUpload);
}

#[tokio::test]
async fn find_by_id_returns_none_when_missing() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    assert!(repo.find_by_id(&VideoId::new()).await.unwrap().is_none());
}

#[tokio::test]
async fn update_status_if_only_updates_on_match() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let video = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();

    // Matching expected status: updates.
    let ok = repo
        .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    assert!(ok);
    let after = repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::Uploaded);

    // Non-matching expected: no-op.
    let ok = repo
        .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Processing)
        .await
        .unwrap();
    assert!(!ok);
    let after = repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::Uploaded);
}

#[tokio::test]
async fn mark_processed_sets_token_and_status_atomically() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let mut video = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();
    repo.update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    repo.update_status_if(&video.id, VideoStatus::Uploaded, VideoStatus::Processing)
        .await
        .unwrap();

    let ok = repo.mark_processed(&video.id, "tok-abcdef").await.unwrap();
    assert!(ok);

    video = repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(video.status, VideoStatus::Processed);
    assert_eq!(video.share_token.as_deref(), Some("tok-abcdef"));

    // Second call is a no-op — no longer in Processing.
    let ok = repo.mark_processed(&video.id, "other").await.unwrap();
    assert!(!ok);
}

#[tokio::test]
async fn find_by_share_token_returns_the_right_video() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let video = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();
    repo.update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    repo.update_status_if(&video.id, VideoStatus::Uploaded, VideoStatus::Processing)
        .await
        .unwrap();
    // Use the first 21 chars of a UUID simple form so we fit the
    // varchar(21) column exactly.
    let token: String = VideoId::new().0.simple().to_string().chars().take(21).collect();
    repo.mark_processed(&video.id, &token).await.unwrap();

    let found = repo.find_by_share_token(&token).await.unwrap().unwrap();
    assert_eq!(found.id, video.id);

    assert!(repo
        .find_by_share_token("definitely-not-a-token")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn find_stale_filters_by_status_and_cutoff() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    // Old: should be returned.
    let mut old = fresh_video(VideoStatus::Uploaded);
    old.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&old).await.unwrap();

    // Recent: excluded by cutoff.
    let recent = fresh_video(VideoStatus::Uploaded);
    repo.insert(&recent).await.unwrap();

    // Processed: excluded by status filter even when old.
    let mut processed = fresh_video(VideoStatus::PendingUpload);
    processed.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&processed).await.unwrap();
    repo.update_status_if(&processed.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    repo.update_status_if(&processed.id, VideoStatus::Uploaded, VideoStatus::Processing)
        .await
        .unwrap();
    let tok: String = VideoId::new().0.simple().to_string().chars().take(21).collect();
    repo.mark_processed(&processed.id, &tok).await.unwrap();

    let stale = repo
        .find_stale(Utc::now() - ChronoDuration::hours(24))
        .await
        .unwrap();

    let ids: Vec<_> = stale.iter().map(|v| v.id.clone()).collect();
    assert!(ids.contains(&old.id));
    assert!(!ids.contains(&recent.id));
    assert!(!ids.contains(&processed.id));
}

#[tokio::test]
async fn bulk_mark_failed_only_touches_rows_in_matching_statuses() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    let mut uploaded = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&uploaded).await.unwrap();
    repo.update_status_if(&uploaded.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();

    let pending = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&pending).await.unwrap();

    repo.bulk_mark_failed(
        &[uploaded.id.clone(), pending.id.clone()],
        &[VideoStatus::Uploaded, VideoStatus::Processing],
    )
    .await
    .unwrap();

    uploaded = repo.find_by_id(&uploaded.id).await.unwrap().unwrap();
    assert_eq!(uploaded.status, VideoStatus::Failed);

    // pending was not in the from_statuses list → untouched.
    let pending = repo.find_by_id(&pending.id).await.unwrap().unwrap();
    assert_eq!(pending.status, VideoStatus::PendingUpload);
}

#[tokio::test]
async fn delete_removes_the_row() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let video = fresh_video(VideoStatus::Failed);
    repo.insert(&video).await.unwrap();
    repo.delete(&video.id).await.unwrap();
    assert!(repo.find_by_id(&video.id).await.unwrap().is_none());
    // Idempotent: deleting again is a no-op.
    repo.delete(&video.id).await.unwrap();
}
