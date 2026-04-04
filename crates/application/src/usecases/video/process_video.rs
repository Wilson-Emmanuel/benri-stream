use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::TranscoderPort;
use domain::ports::video::VideoRepository;
use domain::video::{generate_share_token, quality::QualityLevel, VideoId, VideoStatus};

pub struct ProcessVideoUseCase {
    video_repo: Arc<dyn VideoRepository>,
    storage: Arc<dyn StoragePort>,
    transcoder: Arc<dyn TranscoderPort>,
}

impl ProcessVideoUseCase {
    pub fn new(
        video_repo: Arc<dyn VideoRepository>,
        storage: Arc<dyn StoragePort>,
        transcoder: Arc<dyn TranscoderPort>,
    ) -> Self {
        Self { video_repo, storage, transcoder }
    }

    pub async fn execute(&self, input: Input) -> Result<(), Error> {
        let video = self
            .video_repo
            .find_by_id(&input.video_id)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .ok_or(Error::VideoNotFound)?;

        // Atomically claim: UPLOADED -> PROCESSING
        let claimed = self
            .video_repo
            .update_status_if(&video.id, VideoStatus::Uploaded, VideoStatus::Processing)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;

        if !claimed {
            return Ok(()); // Another worker claimed it
        }

        // Probe
        if let Err(e) = self.transcoder.probe(&video.upload_key).await {
            tracing::error!(video_id = %video.id, error = %e, "probe failed");
            let _ = self.video_repo.update_status_if(&video.id, VideoStatus::Processing, VideoStatus::Failed).await;
            let _ = self.storage.delete_object(&video.upload_key).await;
            return Ok(());
        }

        // Transcode — the on_first_segment callback generates the share token
        let video_repo = self.video_repo.clone();
        let video_id = video.id.clone();

        let on_first_segment = Box::new(move || {
            let repo = video_repo;
            let id = video_id;
            let token = generate_share_token();
            tokio::spawn(async move {
                let _ = repo.set_share_token(&id, &token).await;
                let _ = repo.update_status_if(&id, VideoStatus::Processing, VideoStatus::Partial).await;
                tracing::info!(video_id = %id, "first segment ready, share token generated");
            });
        });

        let output_prefix = video.storage_prefix();
        match self
            .transcoder
            .transcode_to_hls(
                &video.upload_key,
                &output_prefix,
                QualityLevel::all(),
                on_first_segment,
            )
            .await
        {
            Ok(result) => {
                if result.segments_produced > 0 {
                    let _ = self.video_repo.update_status_if(
                        &video.id, VideoStatus::Partial, VideoStatus::Processed
                    ).await;
                } else {
                    let _ = self.video_repo.update_status_if(
                        &video.id, VideoStatus::Processing, VideoStatus::Failed
                    ).await;
                }
            }
            Err(e) => {
                tracing::error!(video_id = %video.id, error = %e, "transcode failed");
                // Check if we produced any segments (PARTIAL status)
                let current = self.video_repo.find_by_id(&video.id).await
                    .map_err(|e| Error::Internal(e.to_string()))?;
                if let Some(v) = current {
                    if v.status == VideoStatus::Partial {
                        // Partial success — keep what we have
                        let _ = self.video_repo.update_status_if(
                            &video.id, VideoStatus::Partial, VideoStatus::Incomplete
                        ).await;
                    } else {
                        let _ = self.video_repo.update_status_if(
                            &video.id, VideoStatus::Processing, VideoStatus::Failed
                        ).await;
                    }
                }
            }
        }

        // Clean up original file
        let _ = self.storage.delete_object(&video.upload_key).await;

        tracing::info!(video_id = %video.id, "processing complete");
        Ok(())
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
