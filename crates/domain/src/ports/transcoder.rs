use async_trait::async_trait;

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait TranscoderPort: Send + Sync {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError>;

    /// Transcode the input into adaptive HLS and upload the resulting
    /// segments + per-tier playlists + master playlist to storage.
    ///
    /// The transcoder fires `first_segment_ready` exactly once, at the
    /// moment the master playlist and the low tier's first segment are
    /// both durably in storage — that's the earliest point at which a
    /// viewer holding a share link could begin playback. The caller
    /// uses this signal to publish the share link without waiting for
    /// the full transcode (which can take minutes on CPU-only hosts).
    ///
    /// If the pipeline errors before the first low segment lands, the
    /// notifier is dropped without being called. The caller must treat
    /// "notifier never fired" as a normal failure outcome, not a bug.
    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        probe: &ProbeResult,
        first_segment_ready: Box<dyn FirstSegmentNotifier>,
    ) -> Result<(), TranscoderError>;
}

/// One-shot signal fired when the master playlist and the low tier's first
/// segment are both in storage. Consumed by value so the at-most-once
/// contract is type-enforced.
pub trait FirstSegmentNotifier: Send + Sync {
    fn notify(self: Box<Self>);
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
