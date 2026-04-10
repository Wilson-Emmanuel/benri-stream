use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use domain::ports::distributed_lock::DistributedLockPort;
use domain::ports::task::TaskRepository;
use domain::task::metadata::cleanup_stale_videos::CleanupStaleVideosTaskMetadata;
use domain::task::scheduler::TaskScheduler;

const CHECK_INTERVAL: Duration = Duration::from_secs(300);
const LOCK_KEY: &str = "benri:task:system_checker:lock";
const LOCK_TTL_SECS: u64 = 60;

/// Bootstraps recurring system tasks. For each registered type, schedules a
/// new instance if no active (`PENDING` or `IN_PROGRESS`) row exists.
/// `Task::compute_update` handles rescheduling after successful completion, so
/// this checker mainly covers cold starts and dead-lettered instances.
pub struct SystemTaskChecker {
    task_repo: Arc<dyn TaskRepository>,
    lock: Arc<dyn DistributedLockPort>,
}

impl SystemTaskChecker {
    pub fn new(
        task_repo: Arc<dyn TaskRepository>,
        lock: Arc<dyn DistributedLockPort>,
    ) -> Self {
        Self { task_repo, lock }
    }

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                tracing::info!("system task checker shutting down");
                return;
            }

            if let Err(e) = self.check_once().await {
                tracing::error!(error = %e, "system task checker error");
            }

            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { return; }
                }
                _ = tokio::time::sleep(CHECK_INTERVAL) => {}
            }
        }
    }

    async fn check_once(&self) -> Result<(), String> {
        let token = match self
            .lock
            .acquire(LOCK_KEY, LOCK_TTL_SECS)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(t) => t,
            None => return Ok(()),
        };

        // On panic the lock expires via its TTL; another instance picks up
        // on the next cycle.
        let result = self.do_check().await;
        let _ = self.lock.release(LOCK_KEY, &token).await;
        result
    }

    async fn do_check(&self) -> Result<(), String> {
        self.ensure_active(CleanupStaleVideosTaskMetadata::METADATA_TYPE, || {
            CleanupStaleVideosTaskMetadata
        })
        .await?;

        Ok(())
    }

    async fn ensure_active<M, F>(&self, metadata_type: &str, make_metadata: F) -> Result<(), String>
    where
        M: domain::task::TaskMetadata + Send + Sync + 'static,
        F: FnOnce() -> M,
    {
        let active = self
            .task_repo
            .count_active_by_type(metadata_type)
            .await
            .map_err(|e| e.to_string())?;

        if active > 0 {
            return Ok(());
        }

        tracing::info!(metadata_type, "creating missing system task");
        let metadata = make_metadata();
        TaskScheduler::schedule_standalone(self.task_repo.as_ref(), &metadata, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
