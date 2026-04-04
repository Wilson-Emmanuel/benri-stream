use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::process_video::{self, ProcessVideoUseCase};
use domain::task::Task;
use domain::task::result::TaskResult;
use domain::video::VideoId;

use super::TaskHandler;

pub struct ProcessVideoHandler {
    use_case: Arc<ProcessVideoUseCase>,
}

impl ProcessVideoHandler {
    pub fn new(use_case: Arc<ProcessVideoUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TaskHandler for ProcessVideoHandler {
    async fn handle(&self, task: &Task) -> TaskResult {
        let video_id: String = task
            .metadata
            .get("video_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let uuid = match uuid::Uuid::parse_str(&video_id) {
            Ok(u) => u,
            Err(_) => {
                return TaskResult::PermanentFailure {
                    error: format!("Invalid video_id in metadata: {}", video_id),
                };
            }
        };

        let input = process_video::Input {
            video_id: VideoId(uuid),
        };

        match self.use_case.execute(input).await {
            Ok(()) => TaskResult::Success { message: None },
            Err(process_video::Error::VideoNotFound) => TaskResult::PermanentFailure {
                error: "Video not found".to_string(),
            },
            Err(process_video::Error::Internal(e)) => TaskResult::RetryableFailure {
                error: e,
            },
        }
    }
}
