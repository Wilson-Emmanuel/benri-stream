use std::sync::Arc;

use domain::ports::error::RepositoryError;
use domain::ports::storage::StoragePort;
use domain::ports::transaction::TransactionPort;
use domain::ports::transcoder::{FirstSegmentNotifier, TranscoderPort};
use domain::ports::video::VideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::scheduler::TaskScheduler;
use domain::video::{generate_share_token, Video, VideoId, VideoStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// UC-VID-005 — Process Video.
///
/// Lifecycle: claim (`Uploaded → Processing`) → probe → transcode →
/// `mark_processed` (`Processing → Processed`).
///
/// The transcoder fires a [`FirstSegmentNotifier`] once the low-tier's
/// first segment and master playlist are in storage. A background task
/// writes the share token at that point so the uploader sees the share
/// link within seconds rather than waiting for the full transcode.
/// `mark_processed` at the end writes the same token value, so the
/// share_token column update is a no-op on the common path.
///
/// On any failure, `Processing → Failed` and a `DeleteVideo` task are
/// committed atomically. A failure after early publish means viewers
/// with the link will get `VIDEO_NOT_FOUND` once the delete runs.
pub struct ProcessVideoUseCase {
    video_repo: Arc<dyn VideoRepository>,
    tx: Arc<dyn TransactionPort>,
    storage: Arc<dyn StoragePort>,
    transcoder: Arc<dyn TranscoderPort>,
}

impl ProcessVideoUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        tx: Arc<dyn TransactionPort>,
        storage: Arc<dyn StoragePort>,
        transcoder: Arc<dyn TranscoderPort>,
    ) -> Self {
        Self { video_repo, tx, storage, transcoder }
    }

    pub async fn execute(&self, input: Input) -> Result<(), Error> {
        tracing::info!(video_id = %input.video_id, "starting video processing");

        let video = self.load_video(&input.video_id).await?;
        if !self.claim_for_processing(&video.id).await? {
            return Ok(()); // Another worker claimed it — nothing to do.
        }

        let probe = match self.transcoder.probe(&video.upload_key).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(video_id = %video.id, error = %e, "probe failed");
                self.fail(&video.id, "probe failed").await;
                return Ok(());
            }
        };

        // Run the transcode and early-publish concurrently.
        // Both use the same token, so the final mark_processed write is a no-op.
        let share_token = generate_share_token();
        let (notifier, first_segment_rx) = make_notifier_pair();
        let publisher = self.spawn_early_publisher(
            video.id.clone(),
            share_token.clone(),
            first_segment_rx,
        );

        let output_prefix = video.storage_prefix();
        let transcode_result = self
            .transcoder
            .transcode_to_hls(&video.upload_key, &output_prefix, &probe, notifier)
            .await;

        // Wait for the publisher to wind down before proceeding.
        let _ = publisher.await;

        if let Err(e) = transcode_result {
            tracing::error!(video_id = %video.id, error = %e, "transcode failed");
            self.fail(&video.id, "transcode failed").await;
            return Ok(());
        }

        self.finalize(&video, &share_token).await;
        Ok(())
    }

    async fn load_video(&self, id: &VideoId) -> Result<Video, Error> {
        self.video_repo
            .find_by_id(id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)
    }

    /// Atomically transitions `Uploaded → Processing`. Returns `false` if another worker claimed it first.
    async fn claim_for_processing(&self, id: &VideoId) -> Result<bool, Error> {
        self.video_repo
            .update_status_if(id, VideoStatus::Uploaded, VideoStatus::Processing)
            .await
            .map_err(|e| Error::Internal(e.to_string()))
    }

    /// Spawns the background task that waits for the first-segment signal and writes the share token.
    fn spawn_early_publisher(
        &self,
        video_id: VideoId,
        share_token: String,
        first_segment_rx: oneshot::Receiver<()>,
    ) -> JoinHandle<()> {
        let video_repo = self.video_repo.clone();
        tokio::spawn(async move {
            if first_segment_rx.await.is_err() {
                // Notifier dropped without firing — pipeline failed before first segment.
                return;
            }
            publish_share_token(video_repo.as_ref(), &video_id, &share_token).await;
        })
    }

    async fn finalize(&self, video: &Video, share_token: &str) {
        match self.video_repo.mark_processed(&video.id, share_token).await {
            Ok(true) => {
                cleanup_original_inline(&self.storage, video).await;
                tracing::info!(video_id = %video.id, "processing complete");
            }
            Ok(false) => {
                // Row was no longer Processing — another path already took over.
                tracing::warn!(
                    video_id = %video.id,
                    "mark_processed found no row in Processing state",
                );
            }
            Err(e) => {
                // Transcode succeeded but the DB flip failed. Fail immediately
                // rather than leaving the row in Processing for the safety net
                // to collect — which would delete otherwise-good segments.
                tracing::error!(
                    video_id = %video.id,
                    error = %e,
                    "failed to mark video processed after successful transcode; failing the video",
                );
                self.fail(&video.id, "mark_processed failed").await;
            }
        }
    }

    async fn fail(&self, video_id: &VideoId, reason: &str) {
        if let Err(tx_err) = fail_and_schedule_delete(&self.tx, video_id).await {
            tracing::error!(
                video_id = %video_id,
                reason,
                error = %tx_err,
                "failed to record failure outcome; safety-net sweep will collect",
            );
        }
    }
}

// --- helpers -----------------------------------------------------------------

/// Bridges [`FirstSegmentNotifier`] to a tokio oneshot channel.
fn make_notifier_pair() -> (Box<dyn FirstSegmentNotifier>, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    (Box::new(OneshotNotifier { tx: Some(tx) }), rx)
}

struct OneshotNotifier {
    tx: Option<oneshot::Sender<()>>,
}

impl FirstSegmentNotifier for OneshotNotifier {
    fn notify(mut self: Box<Self>) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Writes the share token while the video is still `Processing`.
async fn publish_share_token(
    video_repo: &dyn VideoRepository,
    video_id: &VideoId,
    share_token: &str,
) {
    match video_repo.set_share_token(video_id, share_token).await {
        Ok(true) => {
            tracing::info!(
                video_id = %video_id,
                "share link published early (first segment ready)",
            );
        }
        Ok(false) => {
            // Row is no longer Processing — failure path likely won a race.
            tracing::warn!(
                video_id = %video_id,
                "early publish found no row in Processing state",
            );
        }
        Err(e) => {
            // Don't fail the transcode — mark_processed will write the same
            // token and surface any real DB problem.
            tracing::error!(
                video_id = %video_id,
                error = %e,
                "failed to publish share token early; will retry at finalize",
            );
        }
    }
}

/// Atomically transitions `Processing → Failed` and schedules a `DeleteVideo` task.
async fn fail_and_schedule_delete(
    tx: &Arc<dyn TransactionPort>,
    video_id: &VideoId,
) -> Result<(), RepositoryError> {
    let id = video_id.clone();
    tx.run(Box::new(move |scope| {
        Box::pin(async move {
            scope
                .videos()
                .update_status_if(&id, VideoStatus::Processing, VideoStatus::Failed)
                .await?;
            let metadata = DeleteVideoTaskMetadata { video_id: id };
            TaskScheduler::schedule_in_tx(scope.tasks(), &metadata, None).await?;
            Ok(())
        })
    }))
    .await
}

/// Best-effort deletion of the original upload after a successful transcode.
async fn cleanup_original_inline(storage: &Arc<dyn StoragePort>, video: &Video) {
    if let Err(e) = storage.delete_object(&video.upload_key).await {
        tracing::warn!(
            video_id = %video.id,
            upload_key = %video.upload_key,
            error = %e,
            "failed to delete original upload after processing; orphan will be collected by cleanup task",
        );
    }
}

pub struct Input {
    pub video_id: VideoId,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("video not found")]
    VideoNotFound,
    #[error("internal error: {0}")]
    Internal(String),
}
