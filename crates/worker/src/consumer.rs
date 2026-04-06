use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::Instrument;

use domain::ports::task::{TaskConsumer, TaskRepository};
use domain::task::result::TaskResult;
use domain::task::TaskStatus;

use crate::handlers::TaskHandlerInvoker;

const EMPTY_POLL_DELAY: Duration = Duration::from_secs(1);
const POP_ERROR_DELAY: Duration = Duration::from_secs(5);

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

    /// Consume tasks until `shutdown` is set to `true`. On shutdown, any
    /// in-flight task is allowed to finish (bounded by its processing
    /// timeout). No new tasks are popped.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                tracing::info!("task consumer shutting down");
                return;
            }

            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("task consumer shutting down");
                        return;
                    }
                }
                pop_result = self.consumer.pop() => {
                    match pop_result {
                        Ok(Some(task_id)) => {
                            self.process_one(&task_id).await;
                        }
                        Ok(None) => {
                            tokio::time::sleep(EMPTY_POLL_DELAY).await;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "failed to pop from queue");
                            tokio::time::sleep(POP_ERROR_DELAY).await;
                        }
                    }
                }
            }
        }
    }

    async fn process_one(&self, task_id: &domain::task::TaskId) {
        let task = match self.task_repo.find_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                tracing::warn!(task_id = %task_id, "task not found in DB, skipping");
                return;
            }
            Err(e) => {
                tracing::error!(task_id = %task_id, error = %e, "failed to fetch task");
                return;
            }
        };

        if task.status != TaskStatus::InProgress {
            tracing::debug!(
                task_id = %task_id,
                status = ?task.status,
                "task not IN_PROGRESS, skipping (already processed or reset)",
            );
            return;
        }

        // Open a span for the handler invocation. Include the trace_id so
        // logs emitted by the handler are grep-able back to the original
        // request that scheduled this task.
        let span = tracing::info_span!(
            "task_handler",
            task_id = %task.id,
            metadata_type = %task.metadata_type,
            attempt_count = task.attempt_count,
            trace_id = tracing::field::Empty,
        );
        if let Some(ref tid) = task.trace_id {
            span.record("trace_id", tracing::field::display(tid));
        }

        let timeout = task.processing_timeout();
        let handler = self.handler.clone();
        let task_for_handler = task.clone();

        let result = async move {
            match tokio::time::timeout(timeout, handler.dispatch(&task_for_handler)).await {
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
            }
        }
        .instrument(span)
        .await;

        let update = task.compute_update(&result);

        if let Err(e) = self.task_repo.batch_update(&[update]).await {
            tracing::error!(
                task_id = %task.id,
                error = %e,
                "failed to persist task result — stale recovery will reset",
            );
        }

        // Metrics by terminal outcome.
        match &result {
            TaskResult::Success { .. } => {
                metrics::counter!(
                    "task.succeeded",
                    "metadata_type" => task.metadata_type.clone()
                )
                .increment(1);
            }
            TaskResult::PermanentFailure { .. } => {
                metrics::counter!(
                    "task.failed",
                    "metadata_type" => task.metadata_type.clone()
                )
                .increment(1);
            }
            TaskResult::RetryableFailure { .. } => {
                metrics::counter!(
                    "task.retried",
                    "metadata_type" => task.metadata_type.clone()
                )
                .increment(1);
            }
            _ => {}
        }
    }
}
