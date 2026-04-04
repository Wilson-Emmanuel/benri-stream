use std::sync::Arc;

use chrono::Utc;

use domain::ports::task::TaskRepository;
use infrastructure::redis::distributed_lock::DistributedLock;

const RECOVERY_INTERVAL_SECS: u64 = 60;
const STALE_THRESHOLD_SECS: i64 = 320; // default processing timeout (300s) + 20s buffer
const LOCK_KEY: &str = "benri:task:recovery:lock";
const LOCK_TTL_SECS: u64 = 30;

pub struct StaleRecovery {
    task_repo: Arc<dyn TaskRepository>,
    lock: DistributedLock,
}

impl StaleRecovery {
    pub fn new(task_repo: Arc<dyn TaskRepository>, lock: DistributedLock) -> Self {
        Self { task_repo, lock }
    }

    pub async fn run(&self) {
        loop {
            if let Err(e) = self.recover_once().await {
                tracing::error!(error = %e, "stale recovery error");
            }
            tokio::time::sleep(std::time::Duration::from_secs(RECOVERY_INTERVAL_SECS)).await;
        }
    }

    async fn recover_once(&self) -> Result<(), String> {
        let acquired = self.lock.acquire(LOCK_KEY, LOCK_TTL_SECS).await?;
        if !acquired {
            return Ok(());
        }

        let threshold = Utc::now() - chrono::Duration::seconds(STALE_THRESHOLD_SECS);
        let count = self
            .task_repo
            .reset_stale(threshold)
            .await
            .map_err(|e| e.to_string())?;

        if count > 0 {
            tracing::info!(count, "reset stale tasks to PENDING");
        }

        let _ = self.lock.release(LOCK_KEY).await;
        Ok(())
    }
}
