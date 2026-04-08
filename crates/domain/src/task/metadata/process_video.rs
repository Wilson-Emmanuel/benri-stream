use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::TaskMetadata;
use crate::video::VideoId;

/// UC-VID-005 — transcode an uploaded video into HLS segments.
///
/// Scheduled by UC-VID-002 (complete upload) in the same DB transaction as
/// the `PENDING_UPLOAD → UPLOADED` status update. Ordering key
/// `video_process:{id}` serializes multiple attempts on the same video and
/// dedup-by-defaults prevents concurrent processing.
///
/// See `business-spec/task-system/task-catalog.md#processvideo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessVideoTaskMetadata {
    pub video_id: VideoId,
}

impl ProcessVideoTaskMetadata {
    pub const METADATA_TYPE: &'static str = "ProcessVideoTaskMetadata";
}

impl TaskMetadata for ProcessVideoTaskMetadata {
    fn metadata_type_name(&self) -> &'static str {
        Self::METADATA_TYPE
    }

    fn ordering_key(&self) -> Option<String> {
        Some(format!("video_process:{}", self.video_id.0))
    }

    fn max_retries(&self) -> Option<i32> {
        // One attempt, then dead letter. Rationale: the meaningful
        // failure modes for this task are (a) probe failure on a
        // corrupt source — not retryable, the file is bad — and
        // (b) transcode failure mid-pipeline, which in practice is a
        // worker hardware / resource problem that a retry on the same
        // worker won't fix. Retrying also interacts badly with our
        // claim guard: the second attempt would find the video
        // already in `Processing` and no-op, leaving the row stuck.
        // Cleanest to treat any failure as terminal and let the
        // safety-net sweep collect the row.
        Some(1)
    }

    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn processing_timeout(&self) -> Duration {
        // Sized to comfortably fit a 1 GB upload through three CPU
        // tiers of x264 `ultrafast` on typical dev/prod hardware,
        // with ~2× safety margin. 30 minutes (the previous value)
        // was tight enough that a 2 MB webm on a Docker-on-Mac test
        // bed already came within a minute of it. Revisit once
        // hardware encoders are in the pipeline.
        Duration::from_secs(2 * 60 * 60)
    }
}
