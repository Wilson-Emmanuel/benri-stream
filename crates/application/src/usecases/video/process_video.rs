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
/// `mark_processed` (flips `Processing → Processed` and writes the
/// share token).
///
/// **Early share-link publishing**: the transcoder fires a
/// [`FirstSegmentNotifier`] the moment the low tier's first segment
/// and the master playlist are both in storage. A small background
/// task listens for that signal and writes the share token to the
/// video row while the rest of the transcode is still running, so
/// the uploader polling for the share link sees it appear within a
/// few seconds of probe completing rather than minutes later. The
/// final `mark_processed` call at the end of a successful transcode
/// flips the status to `Processed` — idempotent with respect to the
/// share token, which was already written by the early publisher.
///
/// On any failure we transition to `Failed` and schedule a
/// `DeleteVideo` task in the same transaction. If the failure happens
/// *after* the share link has been published, viewers with the link
/// will see `VIDEO_NOT_FOUND` once the delete runs; we accept this
/// narrow race as a reasonable tradeoff for the time-to-share win.
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

        // Run the transcode and the early-publish publisher concurrently.
        // Both consume the same share token value, so publish (early) and
        // mark_processed (final) write the same value and the final write
        // is a no-op for the token column.
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

        // Wait for the publisher to wind down (either it published the
        // token on a first-segment signal, or the sender was dropped
        // because transcode failed before the first segment was ready).
        let _ = publisher.await;

        if let Err(e) = transcode_result {
            tracing::error!(video_id = %video.id, error = %e, "transcode failed");
            self.fail(&video.id, "transcode failed").await;
            return Ok(());
        }

        self.finalize(&video, &share_token).await;
        Ok(())
    }

    // --- steps, in the order `execute` calls them -------------------------

    async fn load_video(&self, id: &VideoId) -> Result<Video, Error> {
        self.video_repo
            .find_by_id(id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)
    }

    /// Atomically transition `Uploaded → Processing`. Returns `false`
    /// if another worker already claimed the video, in which case the
    /// caller should bail out cleanly.
    async fn claim_for_processing(&self, id: &VideoId) -> Result<bool, Error> {
        self.video_repo
            .update_status_if(id, VideoStatus::Uploaded, VideoStatus::Processing)
            .await
            .map_err(|e| Error::Internal(e.to_string()))
    }

    /// Spawn the background task that waits for the transcoder's
    /// first-segment signal and writes the share token to the video
    /// row. The returned handle is awaited at the end of `execute` so
    /// we don't return while the publisher is still in flight.
    fn spawn_early_publisher(
        &self,
        video_id: VideoId,
        share_token: String,
        first_segment_rx: oneshot::Receiver<()>,
    ) -> JoinHandle<()> {
        let video_repo = self.video_repo.clone();
        tokio::spawn(async move {
            if first_segment_rx.await.is_err() {
                // Transcoder dropped the notifier without firing —
                // the pipeline failed before the first segment landed.
                // The outer failure path will mark the video FAILED.
                return;
            }
            publish_share_token(video_repo.as_ref(), &video_id, &share_token).await;
        })
    }

    /// Flip `Processing → Processed` and write the share token. The
    /// token value here is the *same* value the early publisher
    /// already wrote in the common case, so the share_token column
    /// update is a no-op on the common path.
    async fn finalize(&self, video: &Video, share_token: &str) {
        match self.video_repo.mark_processed(&video.id, share_token).await {
            Ok(true) => {
                cleanup_original_inline(&self.storage, video).await;
                tracing::info!(video_id = %video.id, "processing complete");
            }
            Ok(false) => {
                // Status was no longer Processing — another path
                // (e.g. safety-net sweep) already recovered the row.
                // That path owns the lifecycle from here.
                tracing::warn!(
                    video_id = %video.id,
                    "mark_processed found no row in Processing state",
                );
            }
            Err(e) => {
                // Transcode succeeded and segments are uploaded, but
                // the atomic flip to Processed failed. Leaving the row
                // in Processing means the safety net would mark it
                // Failed in 24h and DeleteVideo would nuke the
                // otherwise-good segments. Cleaner to fail immediately
                // so the upload can be retried.
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

/// Bridge between the domain's `FirstSegmentNotifier` trait and a tokio
/// `oneshot` channel. The notifier impl is owned by the transcoder and
/// consumed by-value on `notify`; the receiver is awaited by the
/// early-publish task.
fn make_notifier_pair() -> (Box<dyn FirstSegmentNotifier>, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    (Box::new(OneshotNotifier { tx: Some(tx) }), rx)
}

struct OneshotNotifier {
    tx: Option<oneshot::Sender<()>>,
}

impl FirstSegmentNotifier for OneshotNotifier {
    fn notify(mut self: Box<Self>) {
        // `take` + `send`: the receiver may already be dropped if the
        // outer task bailed out for an unrelated reason. That's fine —
        // send just returns an Err we deliberately ignore.
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Write the share token on the video row while the video is still in
/// `Processing`. Purely a logging helper over `set_share_token` — no
/// business logic beyond deciding what to log at each outcome.
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
            // The video is no longer in `Processing` — most likely
            // the failure path won a race (e.g. transcode errored
            // immediately after the first segment landed). Nothing
            // to do; the outer flow owns the lifecycle.
            tracing::warn!(
                video_id = %video_id,
                "early publish found no row in Processing state",
            );
        }
        Err(e) => {
            // Don't fail the whole transcode on a transient DB hiccup
            // here — the final `mark_processed` will write the same
            // token as part of the success path and surface any real
            // DB problem itself.
            tracing::error!(
                video_id = %video_id,
                error = %e,
                "failed to publish share token early; will retry at finalize",
            );
        }
    }
}

/// Atomically transition `Processing → Failed` and schedule a `DeleteVideo`
/// task in one transaction. If either mutation fails, both roll back and
/// the safety-net sweep eventually collects the video.
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

/// Best-effort inline deletion of the original upload after a successful
/// transcode. Failure leaves an orphan for the safety-net sweep to collect.
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
