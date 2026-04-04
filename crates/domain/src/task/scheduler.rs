use std::sync::Arc;

use chrono::Utc;

use crate::ports::task::TaskRepository;
use super::{Task, TaskId, TaskMetadata, TaskStatus};

pub struct TaskScheduler {
    task_repo: Arc<dyn TaskRepository>,
}

impl TaskScheduler {
    pub fn new(task_repo: Arc<dyn TaskRepository>) -> Self {
        Self { task_repo }
    }

    pub async fn schedule<M: TaskMetadata>(
        &self,
        metadata: &M,
        trace_id: Option<String>,
    ) -> Result<Task, crate::ports::video::RepositoryError> {
        let now = Utc::now();
        let metadata_json = serde_json::to_value(metadata)
            .map_err(|e| crate::ports::video::RepositoryError::Database(e.to_string()))?;

        let task = Task {
            id: TaskId::new(),
            metadata_type: metadata.metadata_type_name().to_string(),
            metadata: metadata_json,
            status: TaskStatus::Pending,
            ordering_key: metadata.ordering_key(),
            trace_id,
            attempt_count: 0,
            next_run_at: now,
            error: None,
            started_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };

        self.task_repo.create(&task).await
    }
}
