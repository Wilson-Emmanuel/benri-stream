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

// ---- find_stale: all three transient statuses ----

#[tokio::test]
async fn find_stale_returns_all_three_transient_statuses() {
    // The query's status filter is `IN ('PENDING_UPLOAD', 'UPLOADED',
    // 'PROCESSING')`. Seed one old row of each and verify every one
    // comes back — a typo in any of the three string literals would
    // break this.
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    let mut pending = fresh_video(VideoStatus::PendingUpload);
    pending.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&pending).await.unwrap();

    let mut uploaded = fresh_video(VideoStatus::Uploaded);
    uploaded.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&uploaded).await.unwrap();

    let mut processing = fresh_video(VideoStatus::Processing);
    processing.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&processing).await.unwrap();

    let stale = repo
        .find_stale(Utc::now() - ChronoDuration::hours(24))
        .await
        .unwrap();
    let ids: Vec<_> = stale.iter().map(|v| v.id.clone()).collect();
    assert!(ids.contains(&pending.id), "PENDING_UPLOAD must be returned");
    assert!(ids.contains(&uploaded.id), "UPLOADED must be returned");
    assert!(
        ids.contains(&processing.id),
        "PROCESSING must be returned"
    );
}

// ---- find_failed_before ----

#[tokio::test]
async fn find_failed_before_returns_only_old_failed_videos() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    // Old FAILED: included.
    let mut old_failed = fresh_video(VideoStatus::Failed);
    old_failed.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&old_failed).await.unwrap();

    // Recent FAILED: excluded by cutoff.
    let recent_failed = fresh_video(VideoStatus::Failed);
    repo.insert(&recent_failed).await.unwrap();

    // Old PENDING_UPLOAD: excluded by status filter.
    let mut old_pending = fresh_video(VideoStatus::PendingUpload);
    old_pending.created_at = Utc::now() - ChronoDuration::hours(48);
    repo.insert(&old_pending).await.unwrap();

    let got = repo
        .find_failed_before(Utc::now() - ChronoDuration::hours(24))
        .await
        .unwrap();
    let ids: Vec<_> = got.iter().map(|v| v.id.clone()).collect();
    assert!(ids.contains(&old_failed.id));
    assert!(!ids.contains(&recent_failed.id));
    assert!(!ids.contains(&old_pending.id));
}

// ---- All VideoFormat values round-trip through the DB ----

#[tokio::test]
async fn all_video_formats_round_trip_through_the_database() {
    // `VideoFormat::as_str` writes to the DB, `VideoFormat::from_str`
    // reads it back — any drift between the two lookup tables or a
    // missing migration enum value would surface here.
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    for format in [
        VideoFormat::Mp4,
        VideoFormat::Webm,
        VideoFormat::Mov,
        VideoFormat::Avi,
        VideoFormat::Mkv,
    ] {
        let video = Video {
            id: VideoId::new(),
            share_token: None,
            title: "t".into(),
            format,
            status: VideoStatus::PendingUpload,
            upload_key: format!("uploads/{}/original", VideoId::new().0),
            created_at: Utc::now(),
        };
        repo.insert(&video).await.unwrap();
        let got = repo.find_by_id(&video.id).await.unwrap().unwrap();
        assert_eq!(got.format, format, "format must round-trip for {format:?}");
    }
}

// ---- All VideoStatus values round-trip through the DB ----

#[tokio::test]
async fn all_video_statuses_round_trip_through_the_database() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);

    // Use update_status_if to drive the row through each state the
    // production code actually uses. Then drop the row into Failed via
    // bulk_mark_failed so every status the row mapper may read from
    // the DB is exercised in one test.
    let video = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&video).await.unwrap();

    let states = [
        (VideoStatus::PendingUpload, VideoStatus::Uploaded),
        (VideoStatus::Uploaded, VideoStatus::Processing),
    ];
    for (from, to) in states {
        assert!(repo.update_status_if(&video.id, from, to).await.unwrap());
        let got = repo.find_by_id(&video.id).await.unwrap().unwrap();
        assert_eq!(got.status, to);
    }

    // Processing → Processed via mark_processed.
    let token: String = VideoId::new().0.simple().to_string().chars().take(21).collect();
    assert!(repo.mark_processed(&video.id, &token).await.unwrap());
    let got = repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(got.status, VideoStatus::Processed);

    // Separately exercise the Failed state via bulk_mark_failed on a
    // different row (mark_processed can't transition away from
    // Processed, and Failed is only reachable from the cleanup sweep).
    let doomed = fresh_video(VideoStatus::PendingUpload);
    repo.insert(&doomed).await.unwrap();
    repo.update_status_if(&doomed.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    repo.bulk_mark_failed(
        std::slice::from_ref(&doomed.id),
        &[VideoStatus::Uploaded],
    )
    .await
    .unwrap();
    let got = repo.find_by_id(&doomed.id).await.unwrap().unwrap();
    assert_eq!(got.status, VideoStatus::Failed);
}

// ---- bulk_mark_failed: empty input ----

#[tokio::test]
async fn bulk_mark_failed_with_empty_ids_is_ok() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    repo.bulk_mark_failed(&[], &[VideoStatus::Uploaded])
        .await
        .unwrap();
}

// ---- update_status_if on missing row ----

#[tokio::test]
async fn update_status_if_returns_false_when_row_missing() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let ok = repo
        .update_status_if(&VideoId::new(), VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();
    assert!(!ok);
}

// ---- mark_processed on missing row ----

#[tokio::test]
async fn mark_processed_returns_false_when_row_missing() {
    let pool = pg_pool().await;
    let repo = PostgresVideoRepository::new(pool);
    let ok = repo.mark_processed(&VideoId::new(), "tok").await.unwrap();
    assert!(!ok);
}
