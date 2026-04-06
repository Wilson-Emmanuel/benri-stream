use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::process_video::{self, ProcessVideoUseCase};
use domain::task::metadata::process_video::ProcessVideoTaskMetadata;
use domain::task::result::TaskResult;

use super::{TaskExecutionContext, TypedTaskHandler};

pub struct ProcessVideoHandler {
    use_case: Arc<ProcessVideoUseCase>,
}

impl ProcessVideoHandler {
    pub fn new(use_case: Arc<ProcessVideoUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TypedTaskHandler for ProcessVideoHandler {
    type Metadata = ProcessVideoTaskMetadata;

    async fn handle(
        &self,
        metadata: &ProcessVideoTaskMetadata,
        _ctx: &TaskExecutionContext,
    ) -> TaskResult {
        let input = process_video::Input { video_id: metadata.video_id.clone() };

        match self.use_case.execute(input).await {
            Ok(()) => TaskResult::Success { message: None, reschedule_after: None },
            Err(process_video::Error::VideoNotFound) => {
                TaskResult::PermanentFailure { error: "video not found".to_string() }
            }
            Err(process_video::Error::Internal(e)) => {
                TaskResult::RetryableFailure { error: e, retry_after: None }
            }
        }
    }
}
