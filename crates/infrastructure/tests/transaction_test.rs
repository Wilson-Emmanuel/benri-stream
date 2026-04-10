#![cfg(feature = "test-support")]

use chrono::Utc;

use domain::ports::error::RepositoryError;
use domain::ports::task::TaskRepository;
use domain::ports::transaction::TransactionPort;
use domain::ports::video::VideoRepository;
use domain::task::metadata::process_video::ProcessVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::postgres::transaction::PgTransactionPort;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::testing::pg_pool;

fn uploaded_video() -> Video {
    Video {
        id: VideoId::new(),
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::PendingUpload,
        upload_key: format!("uploads/{}/original.mp4", VideoId::new().0),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn commit_persists_both_mutations_atomically() {
    let pool = pg_pool().await;
    let video_repo = PostgresVideoRepository::new(pool.clone());
    let task_repo = PostgresTaskRepository::new(pool.clone());
    let tx = PgTransactionPort::new(pool.clone());

    // Seed a PendingUpload video.
    let video = uploaded_video();
    video_repo.insert(&video).await.unwrap();

    let id = video.id.clone();
    tx.run(Box::new(move |scope| {
        Box::pin(async move {
            let ok = scope
                .videos()
                .update_status_if(&id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
                .await?;
            assert!(ok);
            TaskScheduler::schedule_in_tx(
                scope.tasks(),
                &ProcessVideoTaskMetadata { video_id: id.clone() },
                None,
            )
            .await?;
            Ok(())
        })
    }))
    .await
    .unwrap();

    let after = video_repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::Uploaded);

    let count = task_repo
        .count_active_by_type("ProcessVideoTaskMetadata")
        .await
        .unwrap();
    assert!(count >= 1);
}

#[tokio::test]
async fn rollback_on_error_reverts_both_mutations() {
    let pool = pg_pool().await;
    let video_repo = PostgresVideoRepository::new(pool.clone());
    let tx = PgTransactionPort::new(pool.clone());

    let video = uploaded_video();
    video_repo.insert(&video).await.unwrap();

    let id = video.id.clone();
    let result = tx
        .run(Box::new(move |scope| {
            Box::pin(async move {
                scope
                    .videos()
                    .update_status_if(&id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
                    .await?;
                TaskScheduler::schedule_in_tx(
                    scope.tasks(),
                    &ProcessVideoTaskMetadata { video_id: id.clone() },
                    None,
                )
                .await?;
                // Abort — both mutations must roll back.
                Err(RepositoryError::Database("simulated abort".into()))
            })
        }))
        .await;
    assert!(result.is_err());

    let after = video_repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::PendingUpload);
}

#[tokio::test]
async fn rollback_when_claim_races_does_not_leave_orphan_task() {
    // Simulates: another worker already moved the video out of
    // PendingUpload before this tx's update_status_if fires.
    let pool = pg_pool().await;
    let video_repo = PostgresVideoRepository::new(pool.clone());
    let task_repo = PostgresTaskRepository::new(pool.clone());
    let tx = PgTransactionPort::new(pool.clone());

    let video = uploaded_video();
    video_repo.insert(&video).await.unwrap();
    // Push the video out of PendingUpload before the tx runs.
    video_repo
        .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();

    let id = video.id.clone();
    let count_before = task_repo
        .count_active_by_type("ProcessVideoTaskMetadata")
        .await
        .unwrap();

    let result = tx
        .run(Box::new(move |scope| {
            Box::pin(async move {
                let claimed = scope
                    .videos()
                    .update_status_if(&id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
                    .await?;
                // Schedule before checking the claim result — tests the
                // worst case where the insert lands before the rollback.
                TaskScheduler::schedule_in_tx(
                    scope.tasks(),
                    &ProcessVideoTaskMetadata { video_id: id.clone() },
                    None,
                )
                .await?;
                if !claimed {
                    return Err(RepositoryError::Database("lost race".into()));
                }
                Ok(())
            })
        }))
        .await;
    assert!(result.is_err());

    let count_after = task_repo
        .count_active_by_type("ProcessVideoTaskMetadata")
        .await
        .unwrap();
    assert_eq!(count_after, count_before);
}
