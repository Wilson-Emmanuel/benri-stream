use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::cleanup_stale_videos::CleanupStaleVideosUseCase;
use domain::task::metadata::cleanup_stale_videos::CleanupStaleVideosTaskMetadata;
use domain::task::result::TaskResult;

use super::{TaskExecutionContext, TypedTaskHandler};

pub struct CleanupStaleHandler {
    use_case: Arc<CleanupStaleVideosUseCase>,
}

impl CleanupStaleHandler {
    pub fn new(use_case: Arc<CleanupStaleVideosUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TypedTaskHandler for CleanupStaleHandler {
    type Metadata = CleanupStaleVideosTaskMetadata;

    async fn handle(
        &self,
        _metadata: &CleanupStaleVideosTaskMetadata,
        _ctx: &TaskExecutionContext,
    ) -> TaskResult {
        match self.use_case.execute().await {
            Ok(stats) => TaskResult::Success {
                message: Some(format!(
                    "Scheduled deletion: {} pending, {} stuck, {} failed",
                    stats.pending_scheduled, stats.stuck_scheduled, stats.failed_scheduled
                )),
                reschedule_after: None,
            },
            Err(e) => TaskResult::RetryableFailure {
                error: e.to_string(),
                retry_after: None,
            },
        }
    }
}
