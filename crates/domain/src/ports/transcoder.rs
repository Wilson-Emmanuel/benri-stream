use async_trait::async_trait;

use crate::video::quality::QualityLevel;

#[async_trait]
pub trait TranscoderPort: Send + Sync {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError>;

    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        quality_levels: &[QualityLevel],
        on_first_segment: Box<dyn FnOnce() + Send>,
    ) -> Result<TranscodeResult, TranscoderError>;
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

#[derive(Debug, Clone)]
pub struct TranscodeResult {
    pub segments_produced: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscoderError {
    #[error("probe failed: {0}")]
    ProbeFailed(String),
    #[error("transcode failed: {0}")]
    TranscodeFailed(String),
}
