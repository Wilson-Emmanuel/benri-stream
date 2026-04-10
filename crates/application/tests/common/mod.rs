//! Shared test helpers for application-layer unit tests.
//!
//! [`FakeTransactionPort`] is a hand-rolled `TransactionPort` because
//! `mockall` cannot express the HRTB-laden `TxClosure` signature. It runs
//! the closure against in-memory `MockVideoMutations` / `MockTaskMutations`.

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
