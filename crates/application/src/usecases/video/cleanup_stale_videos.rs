use std::sync::Arc;

use chrono::Utc;

use domain::ports::task::TaskRepository;
use domain::ports::video::VideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::{Task, TaskId, TaskMetadata, TaskStatus};
use domain::video::{Video, VideoStatus};

/// UC-VID-006 Cleanup Stale Videos — safety-net sweep.
///
/// Runs daily. Does NOT delete storage objects or rows directly — instead,
/// enumerates qualifying videos and schedules a `DeleteVideo` task per
/// video. All deletion goes through the single delete path (UC-VID-007).
///
/// The primary delete path is use cases scheduling `DeleteVideo` directly
/// on rejection/failure (UC-VID-002, UC-VID-005). This sweep exists to
/// catch videos that slipped through.
///
/// Two SQL statements per sweep:
/// 1. Bulk-mark stuck UPLOADED/PROCESSING videos to FAILED.
/// 2. Bulk-insert one DeleteVideo task per qualifying video (PENDING_UPLOAD,
///    stuck UPLOADED/PROCESSING, old FAILED).
///
/// Multiple DeleteVideo tasks for the same video are safe — handler is
/// idempotent (sees `VideoNotFound` after the first one runs and Skips).
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

        // Stale transient states (PENDING_UPLOAD, UPLOADED, PROCESSING > 24h)
        let stale = self
            .video_repo
            .find_stale(stale_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // Old FAILED videos (> 24h)
        let failed = self
            .video_repo
            .find_failed_before(failed_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        // Bulk transition stuck UPLOADED/PROCESSING → FAILED.
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

        // Build a DeleteVideo task per qualifying video and bulk-insert.
        let to_delete: Vec<&Video> = stale.iter().chain(failed.iter()).collect();
        let tasks: Vec<Task> = to_delete
            .iter()
            .map(|v| build_delete_task(v))
            .collect();

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

/// Build a `Task` row for a `DeleteVideo` task. Mirrors the construction
/// in `TaskScheduler::schedule` but in batch form: this caller doesn't go
/// through the scheduler because the bulk insert path is on
/// `TaskRepository`, not `TaskMutations`.
fn build_delete_task(video: &Video) -> Task {
    let now = Utc::now();
    let metadata = DeleteVideoTaskMetadata { video_id: video.id.clone() };
    let metadata_json = serde_json::to_value(&metadata)
        .expect("DeleteVideoTaskMetadata is always serializable");

    Task {
        id: TaskId::new(),
        metadata_type: metadata.metadata_type_name().to_string(),
        metadata: metadata_json,
        status: TaskStatus::Pending,
        ordering_key: metadata.ordering_key(),
        // TODO: populate from current trace context once OTel is wired.
        trace_id: None,
        attempt_count: 0,
        next_run_at: now,
        error: None,
        started_at: None,
        completed_at: None,
        created_at: now,
        updated_at: now,
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
