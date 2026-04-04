use std::sync::Arc;

use domain::ports::task::TaskRepository;
use infrastructure::redis::distributed_lock::DistributedLock;

const CHECK_INTERVAL_SECS: u64 = 300; // 5 minutes
const LOCK_KEY: &str = "benri:task:system_checker:lock";
const LOCK_TTL_SECS: u64 = 60;

/// Ensures recurring system tasks always exist. If an active (PENDING or IN_PROGRESS)
/// task of a system type is missing, it recreates it.
///
/// TODO: Register system task metadata types here as the task catalog grows.
/// Currently no system tasks are defined — cleanup is triggered by the worker
/// on a simple timer, not via the task system.
pub struct SystemTaskChecker {
    _task_repo: Arc<dyn TaskRepository>,
    lock: DistributedLock,
}

impl SystemTaskChecker {
    pub fn new(task_repo: Arc<dyn TaskRepository>, lock: DistributedLock) -> Self {
        Self { _task_repo: task_repo, lock }
    }

    pub async fn run(&self) {
        loop {
            if let Err(e) = self.check_once().await {
                tracing::error!(error = %e, "system task checker error");
            }
            tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        }
    }

    async fn check_once(&self) -> Result<(), String> {
        let acquired = self.lock.acquire(LOCK_KEY, LOCK_TTL_SECS).await?;
        if !acquired {
            return Ok(());
        }

        // TODO: For each registered system task metadata, check if an active task exists.
        // If not, create one via TaskScheduler.

        let _ = self.lock.release(LOCK_KEY).await;
        Ok(())
    }
}
