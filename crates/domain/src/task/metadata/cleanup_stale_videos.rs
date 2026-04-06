use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::TaskMetadata;

/// UC-VID-006 — daily safety-net sweep.
///
/// Recurring system task. The system task checker recreates it if no active
/// instance exists; on success it reschedules itself after
/// `execution_interval`. Single-instance — the constant ordering key ensures
/// at most one active sweep at a time across the cluster.
///
/// See `business-spec/task-system/task-catalog.md#cleanupstalevideos`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupStaleVideosTaskMetadata;

impl CleanupStaleVideosTaskMetadata {
    pub const METADATA_TYPE: &'static str = "CleanupStaleVideosTaskMetadata";
}

impl TaskMetadata for CleanupStaleVideosTaskMetadata {
    fn metadata_type_name(&self) -> &'static str {
        Self::METADATA_TYPE
    }

    fn ordering_key(&self) -> Option<String> {
        Some("cleanup_stale_videos".to_string())
    }

    fn max_retries(&self) -> Option<i32> {
        Some(3)
    }

    fn retry_base_delay(&self) -> Duration {
        Duration::from_secs(5 * 60)
    }

    fn execution_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(24 * 60 * 60))
    }

    fn processing_timeout(&self) -> Duration {
        Duration::from_secs(30 * 60)
    }

    fn is_system_task(&self) -> bool {
        true
    }
}
