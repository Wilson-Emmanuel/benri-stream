//! Shared test helpers for application-layer unit tests.
//!
//! The main piece here is `FakeTransactionPort` — a hand-rolled
//! `TransactionPort` impl because `mockall` can't express the
//! HRTB-laden `TxClosure` signature. It simply runs the provided
//! closure against a pair of in-memory mock mutation ports
//! (`MockVideoMutations` and `MockTaskMutations`, both generated
//! by mockall on the domain side via the `mock` feature).
//!
//! Usage pattern:
//!
//! ```ignore
//! let mut videos = MockVideoMutations::new();
//! let mut tasks = MockTaskMutations::new();
//! videos.expect_update_status_if().returning(|_, _, _| Ok(true));
//! tasks.expect_create().returning(|t| Ok(t.clone()));
//!
//! let tx = Arc::new(FakeTransactionPort::new(videos, tasks));
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use domain::ports::error::RepositoryError;
use domain::ports::transaction::{
    MockTaskMutations, MockVideoMutations, TaskMutations, TransactionPort, TxClosure, TxScope,
    VideoMutations,
};

pub struct FakeTransactionPort {
    state: Arc<Mutex<FakeTxState>>,
}

struct FakeTxState {
    videos: MockVideoMutations,
    tasks: MockTaskMutations,
}

impl FakeTransactionPort {
    pub fn new(videos: MockVideoMutations, tasks: MockTaskMutations) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeTxState { videos, tasks })),
        }
    }
}

struct FakeTxScope<'a> {
    state: &'a mut FakeTxState,
}

impl TxScope for FakeTxScope<'_> {
    fn videos(&mut self) -> &mut dyn VideoMutations {
        &mut self.state.videos
    }
    fn tasks(&mut self) -> &mut dyn TaskMutations {
        &mut self.state.tasks
    }
}

#[async_trait]
impl TransactionPort for FakeTransactionPort {
    async fn run(&self, f: TxClosure) -> Result<(), RepositoryError> {
        let mut guard = self.state.lock().await;
        let mut scope = FakeTxScope { state: &mut guard };
        f(&mut scope).await
    }
}
