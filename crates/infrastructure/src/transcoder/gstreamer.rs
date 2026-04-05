use async_trait::async_trait;
use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{ProbeResult, TranscodeResult, TranscoderError, TranscoderPort};

use super::quality::QualityLevel;

/// Target HLS segment duration in seconds. Shorter = faster time-to-stream,
/// longer = fewer files and better CDN cache efficiency. 4s is the balance.
const SEGMENT_DURATION_SECS: u32 = 4;

/// GStreamer-based transcoder. Reads from S3 via URL, writes HLS output directly to S3.
/// No local disk involved — workers are stateless.
///
/// TODO: Implement actual GStreamer pipeline using gstreamer-rs.
/// Current implementation is a stub for compilation.
pub struct GstreamerTranscoder {
    _storage: Arc<dyn StoragePort>,
}

impl GstreamerTranscoder {
    pub fn new(storage: Arc<dyn StoragePort>) -> Self {
        Self { _storage: storage }
    }
}

#[async_trait]
impl TranscoderPort for GstreamerTranscoder {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError> {
        // TODO: Build a GStreamer discoverer pipeline to probe the file at the storage URL.
        // Read file headers via the storage presigned URL, extract codec info, duration, resolution.
        tracing::info!(storage_key = %storage_key, "probing video");

        Err(TranscoderError::ProbeFailed(
            "GStreamer transcoder not yet implemented".to_string(),
        ))
    }

    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        _on_first_segment: Box<dyn FnOnce() + Send>,
    ) -> Result<TranscodeResult, TranscoderError> {
        let quality_levels = QualityLevel::all();
        // TODO: Build a GStreamer pipeline:
        //   Source (S3 URL) → Decode → [Encode low, Encode medium, Encode high] → HLS Mux → S3 Sink
        // Call on_first_segment() when the first segment is written for any quality level.
        tracing::info!(
            input_key = %input_key,
            output_prefix = %output_prefix,
            levels = quality_levels.len(),
            "transcoding to HLS"
        );

        Err(TranscoderError::TranscodeFailed(
            "GStreamer transcoder not yet implemented".to_string(),
        ))
    }
}
