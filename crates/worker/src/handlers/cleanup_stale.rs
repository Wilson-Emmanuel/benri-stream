use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::cleanup_stale_videos::CleanupStaleVideosUseCase;
use domain::task::Task;
use domain::task::result::TaskResult;

use super::TaskHandler;

pub struct CleanupStaleHandler {
    use_case: Arc<CleanupStaleVideosUseCase>,
}

impl CleanupStaleHandler {
    pub fn new(use_case: Arc<CleanupStaleVideosUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TaskHandler for CleanupStaleHandler {
    async fn handle(&self, _task: &Task) -> TaskResult {
        match self.use_case.execute().await {
            Ok(stats) => TaskResult::Success {
                message: Some(format!(
                    "Cleaned: {} pending, {} stuck, {} failed",
                    stats.pending_cleaned, stats.stuck_cleaned, stats.failed_cleaned
                )),
            },
            Err(e) => TaskResult::RetryableFailure {
                error: e.to_string(),
            },
        }
    }
}
