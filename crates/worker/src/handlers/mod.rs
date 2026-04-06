pub mod cleanup_stale;
pub mod delete_video;
pub mod process_video;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use domain::task::result::TaskResult;
use domain::task::{Task, TaskId, TaskMetadata};

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

/// Type-erased handler held by the dispatch map. The wrapper deserializes
/// the task's `serde_json::Value` metadata into the concrete type before
/// invoking the `TypedTaskHandler`.
#[async_trait]
pub trait ErasedHandler: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskResult;
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
    async fn invoke(&self, task: &Task) -> TaskResult {
        let metadata: H::Metadata = match serde_json::from_value(task.metadata.clone()) {
            Ok(m) => m,
            Err(e) => {
                return TaskResult::PermanentFailure {
                    error: format!(
                        "failed to deserialize metadata for {}: {}",
                        task.metadata_type, e
                    ),
                }
            }
        };
        let ctx = TaskExecutionContext {
            task_id: task.id.clone(),
            attempt_count: task.attempt_count,
        };
        self.inner.handle(&metadata, &ctx).await
    }
}

/// Dispatches tasks to their handlers by `metadata_type`.
#[async_trait]
pub trait TaskHandlerInvoker: Send + Sync {
    async fn dispatch(&self, task: &Task) -> TaskResult;
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
    async fn dispatch(&self, task: &Task) -> TaskResult {
        match self.handlers.get(&task.metadata_type) {
            Some(h) => h.invoke(task).await,
            None => TaskResult::PermanentFailure {
                error: format!("no handler registered for metadata type: {}", task.metadata_type),
            },
        }
    }
}
