use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use domain::ports::task::TaskRepository;
use domain::ports::video::RepositoryError;
use domain::task::{Task, TaskId, TaskStatus, TaskUpdate};

pub struct PostgresTaskRepository {
    pool: PgPool,
}

impl PostgresTaskRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_task(row: sqlx::postgres::PgRow) -> Task {
    let metadata_str: String = row.get("metadata");
    Task {
        id: TaskId(row.get("id")),
        metadata_type: row.get("metadata_type"),
        metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null),
        status: TaskStatus::from_str(row.get("status")),
        ordering_key: row.get("ordering_key"),
        trace_id: row.get("trace_id"),
        attempt_count: row.get("attempt_count"),
        next_run_at: row.get("next_run_at"),
        error: row.get("error"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[async_trait]
impl TaskRepository for PostgresTaskRepository {
    async fn create(&self, task: &Task) -> Result<Task, RepositoryError> {
        let metadata_str = serde_json::to_string(&task.metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        sqlx::query(
            "INSERT INTO tasks (id, metadata_type, metadata, status, ordering_key, trace_id,
             attempt_count, next_run_at, error, started_at, completed_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(task.id.0)
        .bind(&task.metadata_type)
        .bind(&metadata_str)
        .bind(task.status.as_str())
        .bind(&task.ordering_key)
        .bind(&task.trace_id)
        .bind(task.attempt_count)
        .bind(task.next_run_at)
        .bind(&task.error)
        .bind(task.started_at)
        .bind(task.completed_at)
        .bind(task.created_at)
        .bind(task.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(task.clone())
    }

    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError> {
        sqlx::query("SELECT * FROM tasks WHERE id = $1")
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(row_to_task))
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError> {
        let uuids: Vec<uuid::Uuid> = ids.iter().map(|id| id.0).collect();
        sqlx::query("SELECT * FROM tasks WHERE id = ANY($1)")
            .bind(&uuids)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(row_to_task).collect())
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError> {
        sqlx::query(
            "SELECT * FROM tasks WHERE status = 'PENDING' AND next_run_at <= $1
             ORDER BY next_run_at ASC LIMIT $2",
        )
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(row_to_task).collect())
        .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError> {
        let uuids: Vec<uuid::Uuid> = ids.iter().map(|id| id.0).collect();
        sqlx::query(
            "UPDATE tasks SET status = 'IN_PROGRESS', started_at = $2, updated_at = $2
             WHERE id = ANY($1)",
        )
        .bind(&uuids)
        .bind(started_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }

    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError> {
        for update in updates {
            sqlx::query(
                "UPDATE tasks SET status = $2, attempt_count = $3, next_run_at = $4,
                 error = $5, completed_at = $6, updated_at = $7 WHERE id = $1",
            )
            .bind(update.task_id.0)
            .bind(update.status.as_str())
            .bind(update.attempt_count)
            .bind(update.next_run_at)
            .bind(&update.error)
            .bind(update.completed_at)
            .bind(update.updated_at)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn reset_stale(&self, threshold: DateTime<Utc>) -> Result<i32, RepositoryError> {
        let result = sqlx::query(
            "UPDATE tasks SET status = 'PENDING', started_at = NULL, updated_at = NOW()
             WHERE status = 'IN_PROGRESS' AND started_at < $1",
        )
        .bind(threshold)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() as i32)
    }

    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM tasks WHERE metadata_type = $1 AND status IN ('PENDING', 'IN_PROGRESS')",
        )
        .bind(metadata_type)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(row.0)
    }
}
