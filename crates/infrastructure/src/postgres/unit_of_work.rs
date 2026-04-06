use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Transaction};

use domain::ports::unit_of_work::{TaskMutations, TxScope, UnitOfWork, VideoMutations};
use domain::ports::video::RepositoryError;
use domain::task::Task;
use domain::video::{Video, VideoId, VideoStatus};

use super::task_repository::row_to_task;

pub struct PgUnitOfWork {
    pool: PgPool,
}

impl PgUnitOfWork {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UnitOfWork for PgUnitOfWork {
    async fn begin(&self) -> Result<Box<dyn TxScope>, RepositoryError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(Box::new(PgTxScope { tx: Some(tx) }))
    }
}

/// Transaction scope backed by a sqlx Postgres transaction.
///
/// Holds the transaction in an `Option` so that `commit` can take ownership
/// of the inner `Transaction` via `&mut self`. If the scope is dropped without
/// commit, the `Option` is still `Some` and sqlx's `Drop` impl for `Transaction`
/// triggers a rollback cleanup task.
pub struct PgTxScope {
    tx: Option<Transaction<'static, Postgres>>,
}

impl PgTxScope {
    fn tx_mut(&mut self) -> Result<&mut Transaction<'static, Postgres>, RepositoryError> {
        self.tx
            .as_mut()
            .ok_or_else(|| RepositoryError::Database("transaction already consumed".to_string()))
    }
}

#[async_trait]
impl TxScope for PgTxScope {
    fn videos(&mut self) -> &mut dyn VideoMutations {
        self
    }

    fn tasks(&mut self) -> &mut dyn TaskMutations {
        self
    }

    async fn commit(&mut self) -> Result<(), RepositoryError> {
        let tx = self
            .tx
            .take()
            .ok_or_else(|| RepositoryError::Database("transaction already consumed".to_string()))?;
        tx.commit()
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }
}

#[async_trait]
impl VideoMutations for PgTxScope {
    async fn insert(&mut self, video: &Video) -> Result<(), RepositoryError> {
        let tx = self.tx_mut()?;
        sqlx::query(
            "INSERT INTO videos (id, share_token, title, format, status, upload_key, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(video.id.0)
        .bind(&video.share_token)
        .bind(&video.title)
        .bind(video.format.as_str())
        .bind(video.status.as_str())
        .bind(&video.upload_key)
        .bind(video.created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }

    async fn update_status_if(
        &mut self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError> {
        let tx = self.tx_mut()?;
        let result = sqlx::query(
            "UPDATE videos SET status = $3 WHERE id = $1 AND status = $2",
        )
        .bind(id.0)
        .bind(expected.as_str())
        .bind(new_status.as_str())
        .execute(&mut **tx)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_share_token(&mut self, id: &VideoId, token: &str) -> Result<(), RepositoryError> {
        let tx = self.tx_mut()?;
        sqlx::query("UPDATE videos SET share_token = $2 WHERE id = $1")
            .bind(id.0)
            .bind(token)
            .execute(&mut **tx)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }

    async fn delete(&mut self, id: &VideoId) -> Result<(), RepositoryError> {
        let tx = self.tx_mut()?;
        sqlx::query("DELETE FROM videos WHERE id = $1")
            .bind(id.0)
            .execute(&mut **tx)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl TaskMutations for PgTxScope {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError> {
        let tx = self.tx_mut()?;
        let metadata_str = serde_json::to_string(&task.metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        sqlx::query(
            "INSERT INTO tasks (
                id, metadata_type, metadata, status, ordering_key, trace_id,
                attempt_count, next_run_at, error, started_at, completed_at,
                max_retries, retry_base_delay_ms, execution_interval_ms, processing_timeout_ms,
                created_at, updated_at
             )
             VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13, $14, $15,
                $16, $17
             )",
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
        .bind(task.max_retries)
        .bind(task.retry_base_delay_ms)
        .bind(task.execution_interval_ms)
        .bind(task.processing_timeout_ms)
        .bind(task.created_at)
        .bind(task.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(task.clone())
    }

    async fn find_active_by_ordering_key(
        &mut self,
        metadata_type: &str,
        ordering_key: &str,
    ) -> Result<Option<Task>, RepositoryError> {
        let tx = self.tx_mut()?;
        let row = sqlx::query(
            "SELECT * FROM tasks
             WHERE metadata_type = $1
               AND ordering_key = $2
               AND status IN ('PENDING', 'IN_PROGRESS')
             LIMIT 1",
        )
        .bind(metadata_type)
        .bind(ordering_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(row.map(row_to_task))
    }
}
