use std::sync::Arc;

use async_trait::async_trait;

use application::usecases::video::delete_video::{self, DeleteVideoUseCase};
use domain::task::Task;
use domain::task::result::TaskResult;
use domain::video::VideoId;

use super::TaskHandler;

pub struct DeleteVideoHandler {
    use_case: Arc<DeleteVideoUseCase>,
}

impl DeleteVideoHandler {
    pub fn new(use_case: Arc<DeleteVideoUseCase>) -> Self {
        Self { use_case }
    }
}

#[async_trait]
impl TaskHandler for DeleteVideoHandler {
    async fn handle(&self, task: &Task) -> TaskResult {
        let video_id_str: String = task
            .metadata
            .get("video_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let uuid = match uuid::Uuid::parse_str(&video_id_str) {
            Ok(u) => u,
            Err(_) => {
                return TaskResult::PermanentFailure {
                    error: format!("Invalid video_id in metadata: {}", video_id_str),
                };
            }
        };

        let input = delete_video::Input {
            video_id: VideoId(uuid),
        };

        match self.use_case.execute(input).await {
            Ok(()) => TaskResult::Success { message: None },
            Err(delete_video::Error::VideoNotFound) => TaskResult::Skip {
                reason: "video already deleted".to_string(),
            },
            Err(delete_video::Error::Internal(e)) => TaskResult::RetryableFailure { error: e },
        }
    }
}
