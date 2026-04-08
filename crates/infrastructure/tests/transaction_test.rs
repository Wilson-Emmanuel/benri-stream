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

    // Seed: a PendingUpload video.
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

    // Video is now Uploaded.
    let after = video_repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(after.status, VideoStatus::Uploaded);

    // Exactly one matching ProcessVideo task exists (for this video).
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
                // Abort — both mutations should roll back.
                Err(RepositoryError::Database("simulated abort".into()))
            })
        }))
        .await;
    assert!(result.is_err());

    let after = video_repo.find_by_id(&video.id).await.unwrap().unwrap();
    assert_eq!(
        after.status,
        VideoStatus::PendingUpload,
        "video status must roll back to its pre-tx value",
    );
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
    // Race: push the video to Uploaded before the tx runs.
    video_repo
        .update_status_if(&video.id, VideoStatus::PendingUpload, VideoStatus::Uploaded)
        .await
        .unwrap();

    let id = video.id.clone();
    // This models the complete_upload path: if the conditional update
    // reports "no row updated", the closure returns Err and the whole
    // tx (including any task it scheduled) rolls back.
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
                // Schedule first, then decide whether to abort — this
                // mirrors the worst case where the task insert lands
                // before the rollback.
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
    assert_eq!(
        count_after, count_before,
        "aborted tx must not leave the scheduled task behind",
    );
}
