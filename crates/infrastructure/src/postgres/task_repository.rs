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

    /// Apply N updates as a single atomic SQL statement.
    ///
    /// Uses `UPDATE ... FROM UNNEST(...)` so the whole batch is one trip
    /// to Postgres. Because it's a single statement, Postgres autocommits
    /// it atomically — no enclosing transaction needed.
    ///
    /// `next_run_at` is COALESCEd against the existing row value: a `None`
    /// in `TaskUpdate.next_run_at` means "leave the column alone" rather
    /// than "set it to NULL" (the column is `NOT NULL`). This matters for
    /// terminal outcomes (`Completed`, `DeadLetter`) where `compute_update`
    /// returns `next_run_at: None` because no future run is expected.
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError> {
        if updates.is_empty() {
            return Ok(());
        }

        let ids: Vec<uuid::Uuid> = updates.iter().map(|u| u.task_id.0).collect();
        let statuses: Vec<String> =
            updates.iter().map(|u| u.status.as_str().to_string()).collect();
        let attempt_counts: Vec<i32> = updates.iter().map(|u| u.attempt_count).collect();
        let next_run_ats: Vec<Option<DateTime<Utc>>> =
            updates.iter().map(|u| u.next_run_at).collect();
        let errors: Vec<Option<String>> = updates.iter().map(|u| u.error.clone()).collect();
        let completed_ats: Vec<Option<DateTime<Utc>>> =
            updates.iter().map(|u| u.completed_at).collect();
        let updated_ats: Vec<DateTime<Utc>> = updates.iter().map(|u| u.updated_at).collect();

        sqlx::query(
            r#"
            UPDATE tasks t
            SET status = v.status,
                attempt_count = v.attempt_count,
                next_run_at = COALESCE(v.next_run_at, t.next_run_at),
                error = v.error,
                completed_at = v.completed_at,
                updated_at = v.updated_at
            FROM UNNEST(
                $1::uuid[],
                $2::varchar[],
                $3::int4[],
                $4::timestamptz[],
                $5::text[],
                $6::timestamptz[],
                $7::timestamptz[]
            ) AS v(id, status, attempt_count, next_run_at, error, completed_at, updated_at)
            WHERE t.id = v.id
            "#,
        )
        .bind(&ids)
        .bind(&statuses)
        .bind(&attempt_counts)
        .bind(&next_run_ats)
        .bind(&errors)
        .bind(&completed_ats)
        .bind(&updated_ats)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(())
    }

    /// Reset tasks stuck IN_PROGRESS for longer than the global stale
    /// threshold (1 hour). This is a fixed value, not per-task: every task
    /// type's `processing_timeout` MUST be less than the threshold so a
    /// running handler is never reset by stale recovery. Today the limit
    /// is 30 minutes per task type — see `TaskMetadata` docstring.
    async fn reset_stale(&self) -> Result<i32, RepositoryError> {
        let result = sqlx::query(
            "UPDATE tasks SET status = 'PENDING', started_at = NULL, updated_at = NOW()
             WHERE status = 'IN_PROGRESS'
               AND started_at IS NOT NULL
               AND started_at < NOW() - INTERVAL '1 hour'",
        )
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

    /// Bulk-insert N tasks in a single statement using `INSERT ... SELECT
    /// FROM UNNEST(...)`. One round trip, atomic at the statement level.
    async fn bulk_create(&self, tasks: &[Task]) -> Result<(), RepositoryError> {
        if tasks.is_empty() {
            return Ok(());
        }
        tracing::info!(count = tasks.len(), "db: bulk creating tasks");

        let ids: Vec<uuid::Uuid> = tasks.iter().map(|t| t.id.0).collect();
        let metadata_types: Vec<String> =
            tasks.iter().map(|t| t.metadata_type.clone()).collect();
        let metadatas: Vec<String> = tasks
            .iter()
            .map(|t| serde_json::to_string(&t.metadata))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        let statuses: Vec<&'static str> = tasks.iter().map(|t| t.status.as_str()).collect();
        let ordering_keys: Vec<Option<String>> =
            tasks.iter().map(|t| t.ordering_key.clone()).collect();
        let trace_ids: Vec<Option<String>> = tasks.iter().map(|t| t.trace_id.clone()).collect();
        let attempt_counts: Vec<i32> = tasks.iter().map(|t| t.attempt_count).collect();
        let next_run_ats: Vec<DateTime<Utc>> = tasks.iter().map(|t| t.next_run_at).collect();
        let errors: Vec<Option<String>> = tasks.iter().map(|t| t.error.clone()).collect();
        let started_ats: Vec<Option<DateTime<Utc>>> =
            tasks.iter().map(|t| t.started_at).collect();
        let completed_ats: Vec<Option<DateTime<Utc>>> =
            tasks.iter().map(|t| t.completed_at).collect();
        let created_ats: Vec<DateTime<Utc>> = tasks.iter().map(|t| t.created_at).collect();
        let updated_ats: Vec<DateTime<Utc>> = tasks.iter().map(|t| t.updated_at).collect();

        sqlx::query(
            r#"
            INSERT INTO tasks (
                id, metadata_type, metadata, status, ordering_key, trace_id,
                attempt_count, next_run_at, error, started_at, completed_at,
                created_at, updated_at
            )
            SELECT * FROM UNNEST(
                $1::uuid[],
                $2::varchar[],
                $3::text[],
                $4::varchar[],
                $5::varchar[],
                $6::varchar[],
                $7::int4[],
                $8::timestamptz[],
                $9::text[],
                $10::timestamptz[],
                $11::timestamptz[],
                $12::timestamptz[],
                $13::timestamptz[]
            )
            "#,
        )
        .bind(&ids)
        .bind(&metadata_types)
        .bind(&metadatas)
        .bind(&statuses)
        .bind(&ordering_keys)
        .bind(&trace_ids)
        .bind(&attempt_counts)
        .bind(&next_run_ats)
        .bind(&errors)
        .bind(&started_ats)
        .bind(&completed_ats)
        .bind(&created_ats)
        .bind(&updated_ats)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(())
    }
}
