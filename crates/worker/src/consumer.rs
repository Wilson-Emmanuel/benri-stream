use std::sync::Arc;

use domain::ports::task::{TaskConsumer, TaskRepository};
use domain::task::TaskStatus;
use domain::task::result::TaskResult;

use crate::handlers::TaskHandlerInvoker;

pub struct TaskConsumerLoop {
    consumer: Arc<dyn TaskConsumer>,
    task_repo: Arc<dyn TaskRepository>,
    handler: Arc<dyn TaskHandlerInvoker>,
}

impl TaskConsumerLoop {
    pub fn new(
        consumer: Arc<dyn TaskConsumer>,
        task_repo: Arc<dyn TaskRepository>,
        handler: Arc<dyn TaskHandlerInvoker>,
    ) -> Self {
        Self { consumer, task_repo, handler }
    }

    pub async fn run(&self) {
        loop {
            match self.consumer.pop().await {
                Ok(Some(task_id)) => {
                    let task = match self.task_repo.find_by_id(&task_id).await {
                        Ok(Some(t)) => t,
                        Ok(None) => {
                            tracing::warn!(task_id = %task_id, "task not found in DB, skipping");
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(task_id = %task_id, error = %e, "failed to fetch task");
                            continue;
                        }
                    };

                    if task.status != TaskStatus::InProgress {
                        tracing::debug!(task_id = %task_id, status = ?task.status, "task not IN_PROGRESS, skipping");
                        continue;
                    }

                    let result = self.handler.invoke(&task).await;
                    let update = task.compute_update(&result);

                    if let Err(e) = self.task_repo.batch_update(&[update]).await {
                        tracing::error!(task_id = %task_id, error = %e, "failed to update task result");
                    }

                    match &result {
                        TaskResult::Success { .. } => {
                            metrics::counter!("task.succeeded", "metadata_type" => task.metadata_type.clone()).increment(1);
                        }
                        TaskResult::PermanentFailure { .. } => {
                            metrics::counter!("task.failed", "metadata_type" => task.metadata_type.clone()).increment(1);
                        }
                        _ => {}
                    }
                }
                Ok(None) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to pop from queue");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }
}
