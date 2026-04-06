use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::TaskMetadata;
use crate::video::VideoId;

/// Task metadata for removing a video's storage objects and database record.
/// Scheduled by use cases on rejection/failure paths (UC-VID-002 rejection,
/// UC-VID-005 failure, UC-VID-006 safety-net sweep). Single delete path for
/// video removal.
///
/// Uses ordering_key `video_delete:{id}` — dedup-by-default prevents multiple
/// active delete tasks from being scheduled for the same video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteVideoTaskMetadata {
    pub video_id: VideoId,
}

impl TaskMetadata for DeleteVideoTaskMetadata {
    fn metadata_type_name(&self) -> &'static str {
        "DeleteVideoTaskMetadata"
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
}
