use async_trait::async_trait;
use redis::AsyncCommands;
use uuid::Uuid;

use domain::ports::task::{QueueError, TaskConsumer};
use domain::task::TaskId;

const TASK_QUEUE_KEY: &str = "benri:tasks:queue";

pub struct RedisTaskConsumer {
    client: redis::Client,
}

impl RedisTaskConsumer {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TaskConsumer for RedisTaskConsumer {
    async fn pop(&self) -> Result<Option<TaskId>, QueueError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| QueueError::Internal(e.to_string()))?;

        let result: Option<String> = conn
            .rpop(TASK_QUEUE_KEY, None)
            .await
            .map_err(|e| QueueError::Internal(e.to_string()))?;

        match result {
            Some(id_str) => {
                let uuid = Uuid::parse_str(&id_str)
                    .map_err(|e| QueueError::Internal(e.to_string()))?;
                tracing::debug!(task_id = %uuid, "redis: popped task from queue");
                metrics::counter!("task.received").increment(1);
                Ok(Some(TaskId(uuid)))
            }
            None => Ok(None),
        }
    }
}
