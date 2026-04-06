pub mod process_video;
pub mod cleanup_stale;
pub mod delete_video;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use domain::task::Task;
use domain::task::result::TaskResult;

#[async_trait]
pub trait TaskHandlerInvoker: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskResult;
}

pub struct HandlerDispatch {
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
}

#[async_trait]
pub trait TaskHandler: Send + Sync {
    async fn handle(&self, task: &Task) -> TaskResult;
}

impl HandlerDispatch {
    pub fn new(handlers: HashMap<String, Arc<dyn TaskHandler>>) -> Self {
        Self { handlers }
    }
}

#[async_trait]
impl TaskHandlerInvoker for HandlerDispatch {
    async fn invoke(&self, task: &Task) -> TaskResult {
        match self.handlers.get(&task.metadata_type) {
            Some(handler) => handler.handle(task).await,
            None => TaskResult::PermanentFailure {
                error: format!("No handler for metadata type: {}", task.metadata_type),
            },
        }
    }
}
