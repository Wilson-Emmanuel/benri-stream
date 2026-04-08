use async_trait::async_trait;

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait TranscoderPort: Send + Sync {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError>;

    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        probe: &ProbeResult,
    ) -> Result<(), TranscoderError>;
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    /// Whether the source has at least one audio stream. Captured by
    /// `probe()` so the transcoder doesn't have to re-read the file
    /// headers to find out.
    pub has_audio: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscoderError {
    #[error("probe failed: {0}")]
    ProbeFailed(String),
    #[error("transcode failed: {0}")]
    TranscodeFailed(String),
}
