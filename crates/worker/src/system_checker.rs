use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use domain::ports::distributed_lock::DistributedLockPort;
use domain::ports::task::TaskRepository;
use domain::ports::unit_of_work::UnitOfWork;
use domain::task::metadata::cleanup_stale_videos::CleanupStaleVideosTaskMetadata;
use domain::task::scheduler::TaskScheduler;

const CHECK_INTERVAL: Duration = Duration::from_secs(300);
const LOCK_KEY: &str = "benri:task:system_checker:lock";
const LOCK_TTL_SECS: u64 = 60;

/// Ensures recurring system tasks always exist. For each registered system
/// task type: if no active (`PENDING` or `IN_PROGRESS`) instance exists,
/// schedules a new one.
///
/// The recurring-task machinery in `Task::compute_update` reschedules a
/// completed system task for its next interval, so this checker's main
/// job is bootstrapping missing tasks after a cold start or after a
/// dead-lettered instance.
pub struct SystemTaskChecker {
    task_repo: Arc<dyn TaskRepository>,
    uow: Arc<dyn UnitOfWork>,
    lock: Arc<dyn DistributedLockPort>,
}

impl SystemTaskChecker {
    pub fn new(
        task_repo: Arc<dyn TaskRepository>,
        uow: Arc<dyn UnitOfWork>,
        lock: Arc<dyn DistributedLockPort>,
    ) -> Self {
        Self { task_repo, uow, lock }
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

        let result = self.do_check().await;
        let _ = self.lock.release(LOCK_KEY, &token).await;
        result
    }

    async fn do_check(&self) -> Result<(), String> {
        // Add each new system task type here.
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
        let mut tx = self.uow.begin().await.map_err(|e| e.to_string())?;
        TaskScheduler::schedule(tx.tasks(), &metadata, None)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
