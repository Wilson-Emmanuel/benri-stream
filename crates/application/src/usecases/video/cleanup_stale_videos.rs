use std::sync::Arc;

use chrono::Utc;

use domain::ports::task::TaskRepository;
use domain::ports::video::VideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::task::Task;
use domain::video::{Video, VideoStatus};

/// UC-VID-006 — Cleanup Stale Videos.
///
/// Daily safety-net sweep. Bulk-marks stuck `Uploaded`/`Processing` videos as
/// `Failed`, then schedules a `DeleteVideo` task for every qualifying video
/// (stale transient states + old `Failed` rows). All deletion goes through
/// UC-VID-007. Duplicate tasks are safe — the handler is idempotent.
pub struct CleanupStaleVideosUseCase {
    video_repo: Arc<dyn VideoRepository>,
    task_repo: Arc<dyn TaskRepository>,
}

impl CleanupStaleVideosUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        task_repo: Arc<dyn TaskRepository>,
    ) -> Self {
        Self { video_repo, task_repo }
    }

    pub async fn execute(&self) -> Result<CleanupStats, Error> {
        tracing::info!("running stale video cleanup sweep");

        let now = Utc::now();
        let stale_threshold = now - chrono::Duration::hours(24);
        let failed_threshold = now - chrono::Duration::hours(24);

        let stale = self
            .video_repo
            .find_stale(stale_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        let failed = self
            .video_repo
            .find_failed_before(failed_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        let stuck_ids: Vec<_> = stale
            .iter()
            .filter(|v| {
                matches!(v.status, VideoStatus::Uploaded | VideoStatus::Processing)
            })
            .map(|v| v.id.clone())
            .collect();
        if !stuck_ids.is_empty() {
            self.video_repo
                .bulk_mark_failed(
                    &stuck_ids,
                    &[VideoStatus::Uploaded, VideoStatus::Processing],
                )
                .await
                .map_err(|e| Error::Internal(e.to_string()))?;
        }

        // build_pending_task keeps construction consistent with the single-task path.
        let to_delete: Vec<&Video> = stale.iter().chain(failed.iter()).collect();
        let tasks: Vec<Task> = to_delete
            .iter()
            .map(|v| {
                TaskScheduler::build_pending_task(
                    &DeleteVideoTaskMetadata { video_id: v.id.clone() },
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !tasks.is_empty() {
            self.task_repo
                .bulk_create(&tasks)
                .await
                .map_err(|e| Error::Internal(e.to_string()))?;
        }

        let stats = CleanupStats {
            pending_scheduled: stale
                .iter()
                .filter(|v| matches!(v.status, VideoStatus::PendingUpload))
                .count() as u32,
            stuck_scheduled: stuck_ids.len() as u32,
            failed_scheduled: failed.len() as u32,
        };

        tracing::info!(
            pending_scheduled = stats.pending_scheduled,
            stuck_scheduled = stats.stuck_scheduled,
            failed_scheduled = stats.failed_scheduled,
            "stale video cleanup sweep complete",
        );

        Ok(stats)
    }
}

#[derive(Debug, Default)]
pub struct CleanupStats {
    pub pending_scheduled: u32,
    pub stuck_scheduled: u32,
    pub failed_scheduled: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("internal error: {0}")]
    Internal(String),
}
