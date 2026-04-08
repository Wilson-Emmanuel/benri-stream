#![cfg(feature = "test-support")]

use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use domain::ports::task::TaskRepository;
use domain::task::scheduler::TaskScheduler;
use domain::task::{Task, TaskId, TaskMetadata, TaskStatus, TaskUpdate};
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::testing::pg_pool;

#[derive(Debug, Serialize, Deserialize)]
struct TestMeta {
    ordering_key: Option<String>,
}

impl TaskMetadata for TestMeta {
    fn metadata_type_name(&self) -> &'static str {
        "TestMeta"
    }
    fn ordering_key(&self) -> Option<String> {
        self.ordering_key.clone()
    }
}

fn build(ordering: Option<&str>) -> Task {
    TaskScheduler::build_pending_task(
        &TestMeta {
            ordering_key: ordering.map(|s| s.to_string()),
        },
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn create_then_find_by_id_round_trips() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    let task = build(None);
    repo.create(&task).await.unwrap();

    let got = repo.find_by_id(&task.id).await.unwrap().unwrap();
    assert_eq!(got.id, task.id);
    assert_eq!(got.metadata_type, "TestMeta");
    assert_eq!(got.status, TaskStatus::Pending);
    assert_eq!(got.attempt_count, 0);
}

#[tokio::test]
async fn bulk_create_inserts_many_tasks_in_one_statement() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    let tasks: Vec<Task> = (0..5).map(|_| build(None)).collect();
    repo.bulk_create(&tasks).await.unwrap();

    let ids: Vec<TaskId> = tasks.iter().map(|t| t.id.clone()).collect();
    let found = repo.find_by_ids(&ids).await.unwrap();
    assert_eq!(found.len(), 5);
}

#[tokio::test]
async fn bulk_create_empty_is_ok() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);
    repo.bulk_create(&[]).await.unwrap();
}

#[tokio::test]
async fn find_pending_returns_due_tasks_in_order() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    // Use a fresh ordering-key family so this test only sees its own rows.
    let family = format!("fp:{}", TaskId::new());
    let mut earlier = build(Some(&family));
    let mut later = build(Some(&format!("{family}:b")));
    earlier.next_run_at = Utc::now() - ChronoDuration::seconds(10);
    later.next_run_at = Utc::now() - ChronoDuration::seconds(5);
    repo.create(&earlier).await.unwrap();
    repo.create(&later).await.unwrap();

    let pending = repo.find_pending(100, Utc::now()).await.unwrap();
    let ours: Vec<_> = pending
        .iter()
        .filter(|t| {
            t.ordering_key
                .as_deref()
                .map(|k| k.starts_with(&family))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(ours.len(), 2);
    // Earlier one comes first (older next_run_at).
    assert_eq!(ours[0].id, earlier.id);
    assert_eq!(ours[1].id, later.id);
}

#[tokio::test]
async fn find_pending_blocks_on_in_progress_sibling() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    let key = format!("blk:{}", TaskId::new());
    let first = build(Some(&key));
    let second = build(Some(&key));
    repo.create(&first).await.unwrap();
    repo.create(&second).await.unwrap();

    // Claim the first.
    repo.mark_in_progress(std::slice::from_ref(&first.id), Utc::now())
        .await
        .unwrap();

    // `second` must not be returned — its sibling is in progress.
    let pending = repo.find_pending(100, Utc::now()).await.unwrap();
    assert!(!pending.iter().any(|t| t.id == second.id));
    assert!(!pending.iter().any(|t| t.id == first.id));
}

#[tokio::test]
async fn batch_update_applies_all_changes() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    let t1 = build(None);
    let t2 = build(None);
    repo.create(&t1).await.unwrap();
    repo.create(&t2).await.unwrap();

    let now = Utc::now();
    let updates = vec![
        TaskUpdate {
            task_id: t1.id.clone(),
            status: TaskStatus::Completed,
            attempt_count: 1,
            next_run_at: None,
            error: None,
            completed_at: Some(now),
            updated_at: now,
        },
        TaskUpdate {
            task_id: t2.id.clone(),
            status: TaskStatus::DeadLetter,
            attempt_count: 5,
            next_run_at: None,
            error: Some("exhausted".into()),
            completed_at: Some(now),
            updated_at: now,
        },
    ];
    repo.batch_update(&updates).await.unwrap();

    let got1 = repo.find_by_id(&t1.id).await.unwrap().unwrap();
    assert_eq!(got1.status, TaskStatus::Completed);
    assert_eq!(got1.attempt_count, 1);

    let got2 = repo.find_by_id(&t2.id).await.unwrap().unwrap();
    assert_eq!(got2.status, TaskStatus::DeadLetter);
    assert_eq!(got2.error.as_deref(), Some("exhausted"));
}

#[tokio::test]
async fn reset_stale_revives_stuck_in_progress_tasks() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool.clone());

    let task = build(None);
    repo.create(&task).await.unwrap();

    // Put it IN_PROGRESS with started_at older than the 1h threshold.
    sqlx::query(
        "UPDATE tasks SET status = 'IN_PROGRESS', started_at = $2 WHERE id = $1",
    )
    .bind(task.id.0)
    .bind(Utc::now() - ChronoDuration::hours(2))
    .execute(&pool)
    .await
    .unwrap();

    let count = repo.reset_stale().await.unwrap();
    assert!(count >= 1);

    let after = repo.find_by_id(&task.id).await.unwrap().unwrap();
    assert_eq!(after.status, TaskStatus::Pending);
    assert!(after.started_at.is_none());
}

#[tokio::test]
async fn count_active_by_type_counts_pending_and_in_progress() {
    let pool = pg_pool().await;
    let repo = PostgresTaskRepository::new(pool);

    // Fresh type name so this test doesn't collide with others.
    #[derive(Debug, Serialize, Deserialize)]
    struct UniqMeta;
    impl TaskMetadata for UniqMeta {
        fn metadata_type_name(&self) -> &'static str {
            "CountActiveByTypeTest"
        }
    }

    let a = TaskScheduler::build_pending_task(&UniqMeta, None).unwrap();
    let b = TaskScheduler::build_pending_task(&UniqMeta, None).unwrap();
    repo.create(&a).await.unwrap();
    repo.create(&b).await.unwrap();
    repo.mark_in_progress(std::slice::from_ref(&a.id), Utc::now())
        .await
        .unwrap();

    let n = repo
        .count_active_by_type("CountActiveByTypeTest")
        .await
        .unwrap();
    assert_eq!(n, 2);

    // Complete one; count drops.
    let now = Utc::now();
    repo.batch_update(&[TaskUpdate {
        task_id: a.id.clone(),
        status: TaskStatus::Completed,
        attempt_count: 0,
        next_run_at: None,
        error: None,
        completed_at: Some(now),
        updated_at: now,
    }])
    .await
    .unwrap();

    let n = repo
        .count_active_by_type("CountActiveByTypeTest")
        .await
        .unwrap();
    assert_eq!(n, 1);
}
