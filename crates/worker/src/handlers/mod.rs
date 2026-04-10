pub mod cleanup_stale;
pub mod delete_video;
pub mod process_video;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use domain::task::result::{OutcomeKind, TaskResult};
use domain::task::{Task, TaskId, TaskMetadata, TaskRunOutcome, TaskStatus, TaskUpdate};

/// Execution context passed to handlers alongside their typed metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TaskExecutionContext {
    pub task_id: TaskId,
    pub attempt_count: i32,
}

/// Per-type task handler. Receives a fully-typed metadata reference — no
/// manual JSON extraction.
#[async_trait]
pub trait TypedTaskHandler: Send + Sync {
    type Metadata: TaskMetadata + Send + Sync + 'static;

    async fn handle(
        &self,
        metadata: &Self::Metadata,
        ctx: &TaskExecutionContext,
    ) -> TaskResult;
}

/// Type-erased handler held by the dispatch map. Owns the deserialize →
/// timeout → invoke → compute_update pipeline.
#[async_trait]
pub trait ErasedHandler: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskRunOutcome;
}

/// Adapter that erases a `TypedTaskHandler` into an `ErasedHandler`.
pub struct HandlerAdapter<H: TypedTaskHandler + 'static> {
    inner: Arc<H>,
}

impl<H: TypedTaskHandler + 'static> HandlerAdapter<H> {
    fn new(inner: Arc<H>) -> Self {
        Self { inner }
    }

    /// Wraps a typed handler into a boxed `ErasedHandler` for the dispatch map.
    pub fn wrap(inner: Arc<H>) -> Arc<dyn ErasedHandler> {
        Arc::new(Self::new(inner))
    }
}

#[async_trait]
impl<H: TypedTaskHandler + 'static> ErasedHandler for HandlerAdapter<H> {
    async fn invoke(&self, task: &Task) -> TaskRunOutcome {
        let metadata: H::Metadata = match serde_json::from_value(task.metadata.clone()) {
            Ok(m) => m,
            // Unparseable metadata is a permanent failure; build the
            // dead-letter outcome directly since there's no typed metadata
            // to pass to compute_update.
            Err(e) => {
                return dead_letter_outcome(
                    task,
                    format!("failed to deserialize metadata for {}: {}", task.metadata_type, e),
                );
            }
        };

        let ctx = TaskExecutionContext {
            task_id: task.id.clone(),
            attempt_count: task.attempt_count,
        };

        let result = self.run_with_timeout(&metadata, &ctx).await;
        task.compute_update(&metadata, &result)
    }
}

impl<H: TypedTaskHandler + 'static> HandlerAdapter<H> {
    async fn run_with_timeout(
        &self,
        metadata: &H::Metadata,
        ctx: &TaskExecutionContext,
    ) -> TaskResult {
        let timeout = metadata.processing_timeout();
        match tokio::time::timeout(timeout, self.inner.handle(metadata, ctx)).await {
            Ok(result) => result,
            Err(_elapsed) => {
                tracing::error!(timeout_secs = timeout.as_secs(), "task handler timed out");
                TaskResult::RetryableFailure {
                    error: format!("handler timed out after {:?}", timeout),
                    retry_after: None,
                }
            }
        }
    }
}

fn dead_letter_outcome(task: &Task, error: String) -> TaskRunOutcome {
    let now = Utc::now();
    TaskRunOutcome {
        update: TaskUpdate {
            task_id: task.id.clone(),
            status: TaskStatus::DeadLetter,
            attempt_count: task.attempt_count,
            next_run_at: None,
            error: Some(error),
            completed_at: Some(now),
            updated_at: now,
        },
        kind: OutcomeKind::Failed,
    }
}

/// Routes tasks to their registered handler by `metadata_type`.
#[async_trait]
pub trait TaskHandlerInvoker: Send + Sync {
    async fn dispatch(&self, task: &Task) -> TaskRunOutcome;
}

pub struct HandlerDispatch {
    handlers: HashMap<String, Arc<dyn ErasedHandler>>,
}

impl HandlerDispatch {
    pub fn new(handlers: HashMap<String, Arc<dyn ErasedHandler>>) -> Self {
        Self { handlers }
    }
}

#[async_trait]
impl TaskHandlerInvoker for HandlerDispatch {
    async fn dispatch(&self, task: &Task) -> TaskRunOutcome {
        match self.handlers.get(&task.metadata_type) {
            Some(h) => h.invoke(task).await,
            None => dead_letter_outcome(
                task,
                format!("no handler registered for metadata type: {}", task.metadata_type),
            ),
        }
    }
}
