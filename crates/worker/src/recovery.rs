use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use domain::ports::distributed_lock::DistributedLockPort;
use domain::ports::task::TaskRepository;

const RECOVERY_INTERVAL: Duration = Duration::from_secs(60);
const LOCK_KEY: &str = "benri:task:recovery:lock";
const LOCK_TTL_SECS: u64 = 30;

/// Periodically resets tasks stuck in IN_PROGRESS (crashed worker, network
/// split, etc.) back to PENDING. The stale threshold is 1 hour; all task
/// processing timeouts are capped well below that to avoid resetting live
/// handlers.
pub struct StaleRecovery {
    task_repo: Arc<dyn TaskRepository>,
    lock: Arc<dyn DistributedLockPort>,
}

impl StaleRecovery {
    pub fn new(task_repo: Arc<dyn TaskRepository>, lock: Arc<dyn DistributedLockPort>) -> Self {
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
        let token = match self
            .lock
            .acquire(LOCK_KEY, LOCK_TTL_SECS)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(t) => t,
            None => return Ok(()),
        };

        let reset_result = self.task_repo.reset_stale().await.map_err(|e| e.to_string());

        // On panic the lock expires via its TTL; another instance picks up
        // on the next cycle.
        let _ = self.lock.release(LOCK_KEY, &token).await;

        reset_result.map(|_| ())
    }
}
