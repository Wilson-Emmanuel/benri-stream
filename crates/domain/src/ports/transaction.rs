use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

use crate::ports::error::RepositoryError;
use crate::task::Task;
use crate::video::{VideoId, VideoStatus};

/// A boxed future that borrows from its argument. Used as the return type
/// of closures passed to [`TransactionPort::run`].
pub type TxFuture<'tx> = Pin<Box<dyn Future<Output = Result<(), RepositoryError>> + Send + 'tx>>;

/// The type of a closure that can be passed to [`TransactionPort::run`].
/// The closure receives a mutable reference to a [`TxScope`] bound to the
/// transaction's lifetime and returns a future that runs inside it.
///
/// The closure itself is `'static`. To read data back out after the
/// transaction commits, capture an owned handle like `Arc<AtomicBool>`
/// or `Arc<Mutex<Option<T>>>` and write to it from inside the closure.
pub type TxClosure = Box<
    dyn for<'tx> FnOnce(&'tx mut (dyn TxScope + 'tx)) -> TxFuture<'tx> + Send + 'static,
>;

/// Opens a database transaction for the duration of a closure.
/// Commits if the closure returns `Ok`, rolls back if it returns `Err` or
/// panics.
///
/// Used for bundling multiple writes into a single atomic unit — e.g.
/// "update a row AND schedule a task." Single-statement writes go
/// directly through the pool-backed repository methods (see
/// [`crate::ports::video::VideoRepository`] and
/// [`crate::ports::task::TaskRepository`]).
///
/// Typical usage:
///
/// ```ignore
/// tx_port
///     .run(Box::new(|scope| {
///         Box::pin(async move {
///             scope
///                 .videos()
///                 .update_status_if(&id, Processing, Failed)
///                 .await?;
///             TaskScheduler::schedule_in_tx(scope.tasks(), &metadata, None).await?;
///             Ok(())
///         })
///     }))
///     .await?;
/// ```
///
/// If the closure needs to return data, capture an `Option` or `Vec` in
/// the enclosing scope and write to it from within the closure.
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

/// Transactional video mutations — only the ones that actually need to be
/// bundled with other writes. Single-op video writes (`insert`, `delete`,
/// `update_status_if`, `mark_processed`) live on the pool-backed
/// [`VideoRepository`](crate::ports::video::VideoRepository) and run as
/// plain single-statement updates without an enclosing transaction.
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

/// Transactional task mutations — used when scheduling a task must be
/// atomic with a business mutation. Standalone scheduling (no business
/// mutation to bundle) uses the pool-backed
/// [`TaskRepository`](crate::ports::task::TaskRepository) instead.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait TaskMutations: Send {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError>;
}
