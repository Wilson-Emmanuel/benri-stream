//! End-to-end handler dispatch tests: task row → HandlerDispatch → use case
//! → real DB + real S3 → TaskRunOutcome.
//!
//! Covers `DeleteVideoHandler` and `CleanupStaleHandler`. `ProcessVideoHandler`
//! is tested at the application layer with a mock transcoder.

use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_s3::primitives::ByteStream;
use chrono::{Duration as ChronoDuration, Utc};

use application::usecases::video::{
    cleanup_stale_videos::CleanupStaleVideosUseCase, delete_video::DeleteVideoUseCase,
};
use domain::ports::task::TaskRepository;
use domain::ports::video::VideoRepository;
use domain::task::metadata::cleanup_stale_videos::CleanupStaleVideosTaskMetadata;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::result::OutcomeKind;
use domain::task::scheduler::TaskScheduler;
use domain::task::{TaskMetadata, TaskStatus};
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::testing::{minio_client, minio_endpoint, pg_pool};

use worker::handlers::cleanup_stale::CleanupStaleHandler;
use worker::handlers::delete_video::DeleteVideoHandler;
use worker::handlers::{ErasedHandler, HandlerAdapter, HandlerDispatch, TaskHandlerInvoker};

struct Harness {
    dispatch: HandlerDispatch,
    pool: sqlx::PgPool,
    s3: aws_sdk_s3::Client,
    upload_bucket: String,
    output_bucket: String,
}

async fn harness() -> Harness {
    let pool = pg_pool().await;
    let s3 = minio_client().await;
    let ep = minio_endpoint().await;

    let storage: Arc<dyn domain::ports::storage::StoragePort> = Arc::new(S3StorageClient::new(
        s3.clone(),
        ep.upload_bucket.clone(),
        ep.output_bucket.clone(),
        "http://cdn.test".into(),
    ));
    let video_repo: Arc<dyn VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool.clone()));

    let delete_uc = Arc::new(DeleteVideoUseCase::new(video_repo.clone(), storage.clone()));
    let cleanup_uc = Arc::new(CleanupStaleVideosUseCase::new(
        video_repo.clone(),
        task_repo.clone(),
    ));

    let mut handlers: HashMap<String, Arc<dyn ErasedHandler>> = HashMap::new();
    handlers.insert(
        DeleteVideoTaskMetadata {
            video_id: VideoId::new(),
        }
        .metadata_type_name()
        .to_string(),
        HandlerAdapter::wrap(Arc::new(DeleteVideoHandler::new(delete_uc))),
    );
    handlers.insert(
        CleanupStaleVideosTaskMetadata
            .metadata_type_name()
            .to_string(),
        HandlerAdapter::wrap(Arc::new(CleanupStaleHandler::new(cleanup_uc))),
    );

    Harness {
        dispatch: HandlerDispatch::new(handlers),
        pool,
        s3,
        upload_bucket: ep.upload_bucket.clone(),
        output_bucket: ep.output_bucket.clone(),
    }
}

#[tokio::test]
async fn delete_video_handler_removes_storage_and_row_and_marks_completed() {
    let h = harness().await;
    let repo = PostgresVideoRepository::new(h.pool.clone());

    let id = VideoId::new();
    let upload_key = format!("uploads/{}/original.mp4", id.0);
    let output_prefix = format!("videos/{}/", id.0);

    let video = Video {
        id: id.clone(),
        share_token: None,
        title: "to-delete".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::Failed,
        upload_key: upload_key.clone(),
        created_at: Utc::now(),
    };
    repo.insert(&video).await.unwrap();

    h.s3.put_object()
        .bucket(&h.upload_bucket)
        .key(&upload_key)
        .body(ByteStream::from(b"x".to_vec()))
        .send()
        .await
        .unwrap();
    h.s3.put_object()
        .bucket(&h.output_bucket)
        .key(format!("{output_prefix}master.m3u8"))
        .body(ByteStream::from(b"x".to_vec()))
        .send()
        .await
        .unwrap();

    let task = TaskScheduler::build_pending_task(
        &DeleteVideoTaskMetadata { video_id: id.clone() },
        None,
    )
    .unwrap();

    let outcome = h.dispatch.dispatch(&task).await;

    assert_eq!(outcome.kind, OutcomeKind::Success);
    assert_eq!(outcome.update.status, TaskStatus::Completed);
    assert!(repo.find_by_id(&id).await.unwrap().is_none());
    assert!(h
        .s3
        .head_object()
        .bucket(&h.upload_bucket)
        .key(&upload_key)
        .send()
        .await
        .is_err());
}

#[tokio::test]
async fn delete_video_handler_on_missing_video_skips() {
    let h = harness().await;
    let task = TaskScheduler::build_pending_task(
        &DeleteVideoTaskMetadata {
            video_id: VideoId::new(),
        },
        None,
    )
    .unwrap();

    let outcome = h.dispatch.dispatch(&task).await;
    assert_eq!(outcome.kind, OutcomeKind::Success);
    assert_eq!(outcome.update.status, TaskStatus::Completed);
    assert_eq!(
        outcome.update.error.as_deref(),
        Some("Skipped: video already deleted"),
    );
}

#[tokio::test]
async fn cleanup_stale_handler_schedules_delete_tasks_for_stale_videos() {
    let h = harness().await;
    let video_repo = PostgresVideoRepository::new(h.pool.clone());
    let task_repo = PostgresTaskRepository::new(h.pool.clone());

    let mut stale = Video {
        id: VideoId::new(),
        share_token: None,
        title: "stale".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::PendingUpload,
        upload_key: format!("uploads/{}/original.mp4", VideoId::new().0),
        created_at: Utc::now() - ChronoDuration::hours(48),
    };
    stale.status = VideoStatus::Uploaded;
    sqlx::query(
        "INSERT INTO videos (id, share_token, title, format, status, upload_key, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(stale.id.0)
    .bind(&stale.share_token)
    .bind(&stale.title)
    .bind(stale.format.as_str())
    .bind(stale.status.as_str())
    .bind(&stale.upload_key)
    .bind(stale.created_at)
    .execute(&h.pool)
    .await
    .unwrap();

    let before = task_repo
        .count_active_by_type(
            DeleteVideoTaskMetadata {
                video_id: VideoId::new(),
            }
            .metadata_type_name(),
        )
        .await
        .unwrap();

    let task = TaskScheduler::build_pending_task(&CleanupStaleVideosTaskMetadata, None).unwrap();
    let outcome = h.dispatch.dispatch(&task).await;
    assert_eq!(outcome.kind, OutcomeKind::Success);
    // Recurring task — success reschedules to Pending, not Completed.
    assert_eq!(outcome.update.status, TaskStatus::Pending);
    assert!(outcome.update.next_run_at.is_some());

    let after = video_repo.find_by_id(&stale.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::Failed);

    let after_count = task_repo
        .count_active_by_type(
            DeleteVideoTaskMetadata {
                video_id: VideoId::new(),
            }
            .metadata_type_name(),
        )
        .await
        .unwrap();
    assert!(after_count > before);
}

#[tokio::test]
async fn dispatch_unknown_metadata_type_returns_dead_letter() {
    let h = harness().await;

    let task = domain::task::Task {
        id: domain::task::TaskId::new(),
        metadata_type: "NoSuchType".into(),
        metadata: serde_json::json!({}),
        status: TaskStatus::InProgress,
        ordering_key: None,
        trace_id: None,
        attempt_count: 0,
        next_run_at: Utc::now(),
        error: None,
        started_at: Some(Utc::now()),
        completed_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let outcome = h.dispatch.dispatch(&task).await;
    assert_eq!(outcome.kind, OutcomeKind::Failed);
    assert_eq!(outcome.update.status, TaskStatus::DeadLetter);
    assert!(outcome.update.error.as_deref().unwrap().contains("NoSuchType"));
}

#[tokio::test]
async fn dispatch_bad_metadata_json_dead_letters() {
    let h = harness().await;

    let task = domain::task::Task {
        id: domain::task::TaskId::new(),
        metadata_type: DeleteVideoTaskMetadata {
            video_id: VideoId::new(),
        }
        .metadata_type_name()
        .to_string(),
        metadata: serde_json::json!({ "video_id": "not-a-uuid" }),
        status: TaskStatus::InProgress,
        ordering_key: None,
        trace_id: None,
        attempt_count: 0,
        next_run_at: Utc::now(),
        error: None,
        started_at: Some(Utc::now()),
        completed_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let outcome = h.dispatch.dispatch(&task).await;
    assert_eq!(outcome.kind, OutcomeKind::Failed);
    assert_eq!(outcome.update.status, TaskStatus::DeadLetter);
}
