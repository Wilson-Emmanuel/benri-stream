pub mod cleanup_stale;
pub mod delete_video;
pub mod process_video;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use domain::task::result::TaskResult;
use domain::task::{Task, TaskId, TaskMetadata, TaskStatus, TaskUpdate};

/// Execution context passed to handlers alongside their typed metadata.
#[derive(Debug, Clone)]
pub struct TaskExecutionContext {
    pub task_id: TaskId,
    pub attempt_count: i32,
}

/// Per-type task handler. Receives a fully-typed metadata reference — no
/// manual JSON extraction. Each handler type binds to exactly one metadata
/// type via the associated `Metadata` type, so adapter construction does
/// not require a turbofish.
///
/// Register via `HandlerAdapter::wrap` in the dispatch map.
#[async_trait]
pub trait TypedTaskHandler: Send + Sync {
    type Metadata: TaskMetadata + Send + Sync + 'static;

    async fn handle(
        &self,
        metadata: &Self::Metadata,
        ctx: &TaskExecutionContext,
    ) -> TaskResult;
}

/// Type-erased handler held by the dispatch map. Owns the entire
/// "deserialize → enforce timeout → invoke handler → compute update"
/// pipeline so the consumer doesn't need access to the typed metadata or
/// the per-task scheduling config.
#[async_trait]
pub trait ErasedHandler: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskUpdate;
}

/// Adapter that erases a `TypedTaskHandler` into an `ErasedHandler`.
pub struct HandlerAdapter<H: TypedTaskHandler + 'static> {
    inner: Arc<H>,
}

impl<H: TypedTaskHandler + 'static> HandlerAdapter<H> {
    pub fn new(inner: Arc<H>) -> Self {
        Self { inner }
    }

    /// Convenience: wrap a typed handler into a boxed erased trait object
    /// ready for insertion into the dispatch map.
    pub fn wrap(inner: Arc<H>) -> Arc<dyn ErasedHandler> {
        Arc::new(Self::new(inner))
    }
}

#[async_trait]
impl<H: TypedTaskHandler + 'static> ErasedHandler for HandlerAdapter<H> {
    async fn invoke(&self, task: &Task) -> TaskUpdate {
        let metadata: H::Metadata = match serde_json::from_value(task.metadata.clone()) {
            Ok(m) => m,
            Err(e) => {
                // Bad metadata is a permanent failure — the task can't be
                // retried because its payload is unparseable. We can't call
                // `compute_update` (no typed metadata to pass), so build the
                // dead-letter `TaskUpdate` by hand.
                let now = Utc::now();
                return TaskUpdate {
                    task_id: task.id.clone(),
                    status: TaskStatus::DeadLetter,
                    attempt_count: task.attempt_count,
                    next_run_at: None,
                    error: Some(format!(
                        "failed to deserialize metadata for {}: {}",
                        task.metadata_type, e
                    )),
                    completed_at: Some(now),
                    updated_at: now,
                };
            }
        };

        let ctx = TaskExecutionContext {
            task_id: task.id.clone(),
            attempt_count: task.attempt_count,
        };

        let timeout = metadata.processing_timeout();
        let result = match tokio::time::timeout(timeout, self.inner.handle(&metadata, &ctx)).await
        {
            Ok(r) => r,
            Err(_elapsed) => {
                tracing::error!(
                    timeout_secs = timeout.as_secs(),
                    "task handler timed out",
                );
                TaskResult::RetryableFailure {
                    error: format!("handler timed out after {:?}", timeout),
                    retry_after: None,
                }
            }
        };

        task.compute_update(&metadata, &result)
    }
}

/// Dispatches tasks to their handlers by `metadata_type`.
#[async_trait]
pub trait TaskHandlerInvoker: Send + Sync {
    async fn dispatch(&self, task: &Task) -> TaskUpdate;
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
    async fn dispatch(&self, task: &Task) -> TaskUpdate {
        match self.handlers.get(&task.metadata_type) {
            Some(h) => h.invoke(task).await,
            None => {
                // No handler registered → permanent failure (dead letter).
                let now = Utc::now();
                TaskUpdate {
                    task_id: task.id.clone(),
                    status: TaskStatus::DeadLetter,
                    attempt_count: task.attempt_count,
                    next_run_at: None,
                    error: Some(format!(
                        "no handler registered for metadata type: {}",
                        task.metadata_type
                    )),
                    completed_at: Some(now),
                    updated_at: now,
                }
            }
        }
    }
}
