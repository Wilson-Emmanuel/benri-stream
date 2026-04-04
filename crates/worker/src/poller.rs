use std::sync::Arc;

use chrono::Utc;

use domain::ports::task::{TaskPublisher, TaskRepository};
use infrastructure::redis::distributed_lock::DistributedLock;

const POLL_INTERVAL_SECS: u64 = 5;
const BATCH_SIZE: i32 = 100;
const LOCK_KEY: &str = "benri:task:poller:lock";
const LOCK_TTL_SECS: u64 = 30;

pub struct OutboxPoller {
    task_repo: Arc<dyn TaskRepository>,
    publisher: Arc<dyn TaskPublisher>,
    lock: DistributedLock,
}

impl OutboxPoller {
    pub fn new(
        task_repo: Arc<dyn TaskRepository>,
        publisher: Arc<dyn TaskPublisher>,
        lock: DistributedLock,
    ) -> Self {
        Self { task_repo, publisher, lock }
    }

    pub async fn run(&self) {
        loop {
            if let Err(e) = self.poll_once().await {
                tracing::error!(error = %e, "poller error");
            }
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
    }

    async fn poll_once(&self) -> Result<(), String> {
        let acquired = self.lock.acquire(LOCK_KEY, LOCK_TTL_SECS).await?;
        if !acquired {
            return Ok(());
        }

        let result = self.do_poll().await;
        let _ = self.lock.release(LOCK_KEY).await;
        result
    }

    async fn do_poll(&self) -> Result<(), String> {
        let now = Utc::now();
        let pending = self
            .task_repo
            .find_pending(BATCH_SIZE, now)
            .await
            .map_err(|e| e.to_string())?;

        if pending.is_empty() {
            return Ok(());
        }

        let ids: Vec<_> = pending.iter().map(|t| t.id.clone()).collect();

        // Update-first: mark IN_PROGRESS before publishing
        self.task_repo
            .mark_in_progress(&ids, now)
            .await
            .map_err(|e| e.to_string())?;

        self.publisher
            .publish(&ids)
            .await
            .map_err(|e| e.to_string())?;

        tracing::info!(count = ids.len(), "published tasks to queue");
        Ok(())
    }
}
