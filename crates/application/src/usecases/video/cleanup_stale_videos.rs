use std::sync::Arc;

use chrono::Utc;

use domain::ports::storage::StoragePort;
use domain::ports::video::VideoRepository;
use domain::video::VideoStatus;

pub struct CleanupStaleVideosUseCase {
    video_repo: Arc<dyn VideoRepository>,
    storage: Arc<dyn StoragePort>,
}

impl CleanupStaleVideosUseCase {
    pub fn new(video_repo: Arc<dyn VideoRepository>, storage: Arc<dyn StoragePort>) -> Self {
        Self { video_repo, storage }
    }

    pub async fn execute(&self) -> Result<CleanupStats, Error> {
        let now = Utc::now();
        let stale_threshold = now - chrono::Duration::hours(24);
        let failed_threshold = now - chrono::Duration::days(30);
        let mut stats = CleanupStats::default();

        // Stale transient states (PENDING_UPLOAD, UPLOADED, PROCESSING older than 24h)
        let stale = self
            .video_repo
            .find_stale(stale_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        for video in stale {
            tracing::info!(video_id = %video.id, status = ?video.status, "cleaning up stale video");
            match video.status {
                VideoStatus::PendingUpload => {
                    let _ = self.storage.delete_object(&video.upload_key).await;
                    let _ = self.video_repo.delete(&video.id).await;
                    stats.pending_cleaned += 1;
                }
                VideoStatus::Uploaded | VideoStatus::Processing => {
                    let _ = self.video_repo.update_status_if(
                        &video.id, video.status, VideoStatus::Failed
                    ).await;
                    let _ = self.storage.delete_object(&video.upload_key).await;
                    stats.stuck_cleaned += 1;
                }
                _ => {}
            }
        }

        // Old FAILED videos (30+ days)
        let failed = self
            .video_repo
            .find_failed_before(failed_threshold)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        for video in failed {
            tracing::info!(video_id = %video.id, "deleting old failed video");
            let _ = self.storage.delete_prefix(&video.storage_prefix()).await;
            let _ = self.storage.delete_object(&video.upload_key).await;
            let _ = self.video_repo.delete(&video.id).await;
            stats.failed_cleaned += 1;
        }

        Ok(stats)
    }
}

#[derive(Debug, Default)]
pub struct CleanupStats {
    pub pending_cleaned: u32,
    pub stuck_cleaned: u32,
    pub failed_cleaned: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("internal error: {0}")]
    Internal(String),
}
