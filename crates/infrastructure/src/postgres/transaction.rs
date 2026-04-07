use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Transaction};

use domain::ports::transaction::{
    TaskMutations, TransactionPort, TxClosure, TxScope, VideoMutations,
};
use domain::ports::video::RepositoryError;
use domain::task::Task;
use domain::video::{VideoId, VideoStatus};

/// Postgres-backed implementation of [`TransactionPort`].
///
/// Opens a sqlx transaction on each `run` call, wraps it in a
/// [`PgTxScope`], runs the caller's closure, and commits or rolls back
/// based on the closure's result.
pub struct PgTransactionPort {
    pool: PgPool,
}

impl PgTransactionPort {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TransactionPort for PgTransactionPort {
    async fn run(&self, f: TxClosure) -> Result<(), RepositoryError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        let mut scope = PgTxScope { tx };

        match f(&mut scope).await {
            Ok(()) => scope
                .tx
                .commit()
                .await
                .map_err(|e| RepositoryError::Database(e.to_string())),
            Err(e) => {
                // Explicit rollback for clarity. On drop sqlx also rolls
                // back, but explicit is nicer when reasoning about the
                // control flow.
                let _ = scope.tx.rollback().await;
                Err(e)
            }
        }
    }
}

/// A concrete `TxScope` backed by a sqlx transaction. Dereferences to the
/// mutation trait impls below — `videos()` and `tasks()` both return
/// `&mut self` typed at the trait, because this same struct implements
/// both `VideoMutations` and `TaskMutations`.
pub struct PgTxScope {
    tx: Transaction<'static, Postgres>,
}

impl TxScope for PgTxScope {
    fn videos(&mut self) -> &mut dyn VideoMutations {
        self
    }

    fn tasks(&mut self) -> &mut dyn TaskMutations {
        self
    }
}

#[async_trait]
impl VideoMutations for PgTxScope {
    async fn update_status_if(
        &mut self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError> {
        tracing::info!(
            video_id = %id,
            expected = ?expected,
            new_status = ?new_status,
            "db[tx]: conditional video status update",
        );
        let result =
            sqlx::query("UPDATE videos SET status = $3 WHERE id = $1 AND status = $2")
                .bind(id.0)
                .bind(expected.as_str())
                .bind(new_status.as_str())
                .execute(&mut *self.tx)
                .await
                .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }
}

#[async_trait]
impl TaskMutations for PgTxScope {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError> {
        tracing::info!(
            task_id = %task.id,
            metadata_type = %task.metadata_type,
            ordering_key = ?task.ordering_key,
            "db[tx]: creating task",
        );
        let metadata_str = serde_json::to_string(&task.metadata)
            .map_err(|e| RepositoryError::Database(e.to_string()))?;

        sqlx::query(
            "INSERT INTO tasks (
                id, metadata_type, metadata, status, ordering_key, trace_id,
                attempt_count, next_run_at, error, started_at, completed_at,
                created_at, updated_at
             )
             VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13
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
        .bind(task.created_at)
        .bind(task.updated_at)
        .execute(&mut *self.tx)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;

        Ok(task.clone())
    }
}
