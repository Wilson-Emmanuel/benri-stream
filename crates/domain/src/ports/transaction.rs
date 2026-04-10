use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

use crate::ports::error::RepositoryError;
use crate::task::Task;
use crate::video::{VideoId, VideoStatus};

/// A boxed future that borrows from its argument. Used as the return type
/// of closures passed to [`TransactionPort::run`].
pub type TxFuture<'tx> = Pin<Box<dyn Future<Output = Result<(), RepositoryError>> + Send + 'tx>>;

/// Closure passed to [`TransactionPort::run`]. Receives a mutable reference
/// to a [`TxScope`] and returns a future that runs inside the transaction.
///
/// To read data back out after commit, capture an `Arc<Mutex<Option<T>>>`
/// in the enclosing scope and write to it from within the closure.
pub type TxClosure = Box<
    dyn for<'tx> FnOnce(&'tx mut (dyn TxScope + 'tx)) -> TxFuture<'tx> + Send + 'static,
>;

/// Runs a closure inside a database transaction, committing on `Ok` and
/// rolling back on `Err` or panic.
///
/// Single-statement writes go directly through the pool-backed repository
/// methods ([`crate::ports::video::VideoRepository`],
/// [`crate::ports::task::TaskRepository`]).
///
/// ```ignore
/// tx_port
///     .run(Box::new(|scope| {
///         Box::pin(async move {
///             scope.videos().update_status_if(&id, Processing, Failed).await?;
///             TaskScheduler::schedule_in_tx(scope.tasks(), &metadata, None).await?;
///             Ok(())
///         })
///     }))
///     .await?;
/// ```
#[async_trait]
pub trait TransactionPort: Send + Sync {
    async fn run(&self, f: TxClosure) -> Result<(), RepositoryError>;
}

/// A single open transaction. Provides access to mutation traits that must
/// share the same database connection / transaction handle.
pub trait TxScope: Send {
    fn videos(&mut self) -> &mut dyn VideoMutations;
    fn tasks(&mut self) -> &mut dyn TaskMutations;
}

/// Video mutations that must be bundled with other writes in a transaction.
/// Single-statement writes live on the pool-backed
/// [`VideoRepository`](crate::ports::video::VideoRepository).
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait VideoMutations: Send {
    /// Atomically set status only if current status matches `expected`.
    /// Returns `true` if a row was updated.
    async fn update_status_if(
        &mut self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError>;
}

/// Task mutations that must be atomic with a business write. Standalone
/// scheduling uses the pool-backed
/// [`TaskRepository`](crate::ports::task::TaskRepository).
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait TaskMutations: Send {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError>;
}
