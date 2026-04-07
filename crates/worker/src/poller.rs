use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::watch;

use domain::ports::distributed_lock::DistributedLockPort;
use domain::ports::task::{TaskPublisher, TaskRepository};

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const BATCH_SIZE: i32 = 100;
const LOCK_KEY: &str = "benri:task:poller:lock";
const LOCK_TTL_SECS: u64 = 30;

pub struct OutboxPoller {
    task_repo: Arc<dyn TaskRepository>,
    publisher: Arc<dyn TaskPublisher>,
    lock: Arc<dyn DistributedLockPort>,
}

impl OutboxPoller {
    pub fn new(
        task_repo: Arc<dyn TaskRepository>,
        publisher: Arc<dyn TaskPublisher>,
        lock: Arc<dyn DistributedLockPort>,
    ) -> Self {
        Self { task_repo, publisher, lock }
    }

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                tracing::info!("outbox poller shutting down");
                return;
            }

            if let Err(e) = self.poll_once().await {
                tracing::error!(error = %e, "poller error");
            }

            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return; }
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    async fn poll_once(&self) -> Result<(), String> {
        let token = match self
            .lock
            .acquire(LOCK_KEY, LOCK_TTL_SECS)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(t) => t,
            None => return Ok(()),
        };

        // The release runs on the normal Ok/Err path. If `do_poll` panics
        // the release is skipped and the lock waits out the TTL — short
        // enough (LOCK_TTL_SECS) that another instance picks up on the
        // next interval. A drop-guard would be tidier but mixing async
        // release with sync `Drop` is awkward; the TTL fallback is the
        // standard pattern for Redis locks of this style.
        let result = self.do_poll().await;
        let _ = self.lock.release(LOCK_KEY, &token).await;
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

        // Update-first: mark IN_PROGRESS before publishing. If publish
        // fails, stale recovery will reset them to PENDING.
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
