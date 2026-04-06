use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::TaskMetadata;
use crate::video::VideoId;

/// UC-VID-007 — single delete path for a video.
///
/// Scheduled from: UC-VID-002 rejection, UC-VID-005 failure, UC-VID-006
/// safety-net sweep. Dedup-by-default prevents multiple active delete tasks
/// per video.
///
/// See `business-spec/task-system/task-catalog.md#deletevideo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteVideoTaskMetadata {
    pub video_id: VideoId,
}

impl DeleteVideoTaskMetadata {
    pub const METADATA_TYPE: &'static str = "DeleteVideoTaskMetadata";
}

impl TaskMetadata for DeleteVideoTaskMetadata {
    fn metadata_type_name(&self) -> &'static str {
        Self::METADATA_TYPE
    }

    fn ordering_key(&self) -> Option<String> {
        Some(format!("video_delete:{}", self.video_id.0))
    }

    fn max_retries(&self) -> Option<i32> {
        Some(5)
    }

    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(5 * 60)
    }
}
