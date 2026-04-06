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
        Some(5)
    }

    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(30 * 60)
    }
}
