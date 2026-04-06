use std::sync::Arc;

use chrono::Utc;

use domain::ports::unit_of_work::UnitOfWork;
use domain::ports::video::{RepositoryError, VideoRepository};
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{VideoId, VideoStatus};

/// UC-VID-006 Cleanup Stale Videos — safety-net sweep.
///
/// Runs daily. Does NOT delete storage objects or rows directly — instead,
/// enumerates qualifying videos and schedules a `DeleteVideo` task per video.
/// All deletion goes through the single delete path (UC-VID-007) so retries,
/// ordering, and dedup are uniform across the system.
///
/// The primary delete path is use cases scheduling `DeleteVideo` directly on
/// rejection/failure (UC-VID-002, UC-VID-005). This sweep exists to catch
/// videos that slipped through — e.g. worker crashed before a direct schedule
/// call, or the task system was unavailable at that moment.
///
/// `TaskScheduler::schedule` is dedup-by-default on the video_id ordering key,
/// so repeated sweeps never create duplicate delete tasks for the same video.
pub struct CleanupStaleVideosUseCase {
    video_repo: Arc<dyn VideoRepository>,
    uow: Arc<dyn UnitOfWork>,
}

impl CleanupStaleVideosUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, uow: Arc<dyn UnitOfWork>) -> Self {
        Self { video_repo, uow }
    }

    pub async fn execute(&self) -> Result<CleanupStats, Error> {
        let now = Utc::now();
        let stale_threshold = now - chrono::Duration::hours(24);
        let failed_threshold = now - chrono::Duration::hours(24);
        let mut stats = CleanupStats::default();

        // Stale transient states (PENDING_UPLOAD, UPLOADED, PROCESSING older than 24h)
        let stale = self
            .video_repo
            .find_stale(stale_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        for video in stale {
            tracing::info!(video_id = %video.id, status = ?video.status, "sweeping stale video");
            match video.status {
                VideoStatus::PendingUpload => {
                    if let Err(e) = schedule_delete(&self.uow, &video.id).await {
                        tracing::warn!(
                            video_id = %video.id,
                            error = %e,
                            "failed to schedule DeleteVideo for stale PENDING_UPLOAD",
                        );
                        continue;
                    }
                    stats.pending_scheduled += 1;
                }
                VideoStatus::Uploaded | VideoStatus::Processing => {
                    // One tx: atomic status→Failed + schedule DeleteVideo.
                    if let Err(e) =
                        fail_and_schedule_delete(&self.uow, &video.id, video.status).await
                    {
                        tracing::warn!(
                            video_id = %video.id,
                            status = ?video.status,
                            error = %e,
                            "failed to record stuck-video outcome",
                        );
                        continue;
                    }
                    stats.stuck_scheduled += 1;
                }
                _ => {}
            }
        }

        // Old FAILED videos (24h+)
        let failed = self
            .video_repo
            .find_failed_before(failed_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        for video in failed {
            tracing::info!(video_id = %video.id, "sweeping old failed video");
            if let Err(e) = schedule_delete(&self.uow, &video.id).await {
                tracing::warn!(
                    video_id = %video.id,
                    error = %e,
                    "failed to schedule DeleteVideo for old FAILED video",
                );
                continue;
            }
            stats.failed_scheduled += 1;
        }

        Ok(stats)
    }
}

async fn schedule_delete(
    uow: &Arc<dyn UnitOfWork>,
    video_id: &VideoId,
) -> Result<(), RepositoryError> {
    let mut tx = uow.begin().await?;
    TaskScheduler::schedule(
        tx.tasks(),
        &DeleteVideoTaskMetadata { video_id: video_id.clone() },
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn fail_and_schedule_delete(
    uow: &Arc<dyn UnitOfWork>,
    video_id: &VideoId,
    current_status: VideoStatus,
) -> Result<(), RepositoryError> {
    let mut tx = uow.begin().await?;
    tx.videos()
        .update_status_if(video_id, current_status, VideoStatus::Failed)
        .await?;
    TaskScheduler::schedule(
        tx.tasks(),
        &DeleteVideoTaskMetadata { video_id: video_id.clone() },
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
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
