use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::watch;

use domain::ports::task::TaskRepository;
use infrastructure::redis::distributed_lock::DistributedLock;

const RECOVERY_INTERVAL: Duration = Duration::from_secs(60);
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

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                tracing::info!("stale recovery shutting down");
                return;
            }

            if let Err(e) = self.recover_once().await {
                tracing::error!(error = %e, "stale recovery error");
            }

            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return; }
                }
                _ = tokio::time::sleep(RECOVERY_INTERVAL) => {}
            }
        }
    }

    async fn recover_once(&self) -> Result<(), String> {
        let token = match self.lock.acquire(LOCK_KEY, LOCK_TTL_SECS).await? {
            Some(t) => t,
            None => return Ok(()),
        };

        let threshold = Utc::now() - chrono::Duration::seconds(STALE_THRESHOLD_SECS);
        let reset_result = self
            .task_repo
            .reset_stale(threshold)
            .await
            .map_err(|e| e.to_string());

        let _ = self.lock.release(LOCK_KEY, &token).await;

        match reset_result {
            Ok(count) if count > 0 => {
                tracing::info!(count, "reset stale tasks to PENDING");
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
