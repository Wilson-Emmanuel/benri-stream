use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::delete_video::{self, DeleteVideoUseCase};
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::result::TaskResult;

use super::{TaskExecutionContext, TypedTaskHandler};

pub struct DeleteVideoHandler {
    use_case: Arc<DeleteVideoUseCase>,
}

impl DeleteVideoHandler {
    pub fn new(use_case: Arc<DeleteVideoUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TypedTaskHandler for DeleteVideoHandler {
    type Metadata = DeleteVideoTaskMetadata;

    async fn handle(
        &self,
        metadata: &DeleteVideoTaskMetadata,
        _ctx: &TaskExecutionContext,
    ) -> TaskResult {
        let input = delete_video::Input { video_id: metadata.video_id.clone() };

        match self.use_case.execute(input).await {
            Ok(()) => TaskResult::Success { message: None, reschedule_after: None },
            Err(delete_video::Error::VideoNotFound) => TaskResult::Skip {
                reason: "video already deleted".to_string(),
            },
            Err(delete_video::Error::Internal(e)) => {
                TaskResult::RetryableFailure { error: e, retry_after: None }
            }
        }
    }
}
