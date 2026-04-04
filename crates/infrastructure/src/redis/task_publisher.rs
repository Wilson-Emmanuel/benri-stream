use async_trait::async_trait;
use redis::AsyncCommands;

use domain::ports::task::{QueueError, TaskPublisher};
use domain::task::TaskId;

const TASK_QUEUE_KEY: &str = "benri:tasks:queue";

pub struct RedisTaskPublisher {
    client: redis::Client,
}

impl RedisTaskPublisher {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TaskPublisher for RedisTaskPublisher {
    async fn publish(&self, task_ids: &[TaskId]) -> Result<bool, QueueError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| QueueError::Internal(e.to_string()))?;

        for id in task_ids {
            let _: () = conn
                .lpush(TASK_QUEUE_KEY, id.0.to_string())
                .await
                .map_err(|e| QueueError::Internal(e.to_string()))?;
        }

        metrics::counter!("task.published").increment(task_ids.len() as u64);
        Ok(true)
    }
}
