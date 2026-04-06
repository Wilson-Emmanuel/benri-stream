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

pub(super) fn row_to_task(row: sqlx::postgres::PgRow) -> Task {
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
        max_retries: row.get("max_retries"),
        retry_base_delay_ms: row.get("retry_base_delay_ms"),
        execution_interval_ms: row.get("execution_interval_ms"),
        processing_timeout_ms: row.get("processing_timeout_ms"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[async_trait]
impl TaskRepository for PostgresTaskRepository {
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

    /// Returns the next batch of PENDING tasks eligible to run.
    ///
    /// Respects ordering keys in SQL (previous implementation ignored them):
    /// 1. Tasks without an ordering key are always eligible.
    /// 2. Tasks with an ordering key whose sibling is IN_PROGRESS are skipped.
    /// 3. For tasks with an ordering key, only the oldest per key is returned.
    ///
    /// Ordered by `next_run_at ASC, id ASC`, capped at `limit`. Two-phase:
    /// select IDs via CTE, then re-fetch full rows through `row_to_task`.
    async fn find_pending(
        &self,
        limit: i32,
        before: DateTime<Utc>,
    ) -> Result<Vec<Task>, RepositoryError> {
        let id_rows = sqlx::query(
            r#"
            WITH blocked_keys AS (
                SELECT DISTINCT ordering_key FROM tasks
                WHERE status = 'IN_PROGRESS' AND ordering_key IS NOT NULL
            ),
            eligible AS (
                SELECT t.id, t.next_run_at, t.ordering_key
                FROM tasks t
                WHERE t.status = 'PENDING'
                  AND t.next_run_at <= $1
                  AND (
                      t.ordering_key IS NULL
                      OR NOT EXISTS (
                          SELECT 1 FROM blocked_keys b
                          WHERE b.ordering_key = t.ordering_key
                      )
                  )
            ),
            one_per_key AS (
                SELECT DISTINCT ON (ordering_key) id, next_run_at
                FROM eligible
                WHERE ordering_key IS NOT NULL
                ORDER BY ordering_key, next_run_at ASC, id ASC
            ),
            no_key AS (
                SELECT id, next_run_at
                FROM eligible
                WHERE ordering_key IS NULL
            )
            SELECT id
            FROM (
                SELECT id, next_run_at FROM one_per_key
                UNION ALL
                SELECT id, next_run_at FROM no_key
            ) combined
            ORDER BY next_run_at ASC, id ASC
            LIMIT $2
            "#,
        )
        .bind(before)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        let ids: Vec<uuid::Uuid> = id_rows.into_iter().map(|r| r.get("id")).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Preserve the order from the CTE result.
        let rows = sqlx::query("SELECT * FROM tasks WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        let mut by_id: std::collections::HashMap<uuid::Uuid, Task> = rows
            .into_iter()
            .map(|r| {
                let t = row_to_task(r);
                (t.id.0, t)
            })
            .collect();

        let ordered: Vec<Task> = ids.into_iter().filter_map(|id| by_id.remove(&id)).collect();
        Ok(ordered)
    }

    async fn mark_in_progress(
        &self,
        ids: &[TaskId],
        started_at: DateTime<Utc>,
    ) -> Result<(), RepositoryError> {
        tracing::info!(count = ids.len(), "db: marking tasks in progress");
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
        let count = result.rows_affected() as i32;
        if count > 0 {
            tracing::info!(count, "db: reset stale tasks to PENDING");
        }
        Ok(count)
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
