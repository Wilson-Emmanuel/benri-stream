use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::Instrument;

use domain::ports::task::{TaskConsumer, TaskRepository};
use domain::task::{Task, TaskId, TaskStatus};

use crate::handlers::{OutcomeKind, TaskHandlerInvoker};

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

    async fn process_one(&self, task_id: &TaskId) {
        let task: Arc<Task> = match self.task_repo.find_by_id(task_id).await {
            Ok(Some(t)) => Arc::new(t),
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

        // Dispatcher owns timeout enforcement and compute_update — the
        // consumer just hands it the task and writes the resulting update.
        let handler = self.handler.clone();
        let task_for_dispatch = task.clone();
        let outcome = async move { handler.dispatch(&task_for_dispatch).await }
            .instrument(span)
            .await;

        let metadata_type = task.metadata_type.clone();
        let outcome_kind = outcome.kind;

        if let Err(e) = self.task_repo.batch_update(&[outcome.update]).await {
            tracing::error!(
                task_id = %task.id,
                error = %e,
                "failed to persist task result — stale recovery will reset",
            );
        }

        // Metric labeling derives from the typed outcome kind, NOT from
        // update.status. update.status is ambiguous (`Pending` could be
        // either a successful recurring reschedule OR a retry from
        // failure). The dispatcher knows the original TaskResult variant
        // and produces the correct OutcomeKind in one place.
        match outcome_kind {
            OutcomeKind::Success => {
                metrics::counter!("task.succeeded", "metadata_type" => metadata_type)
                    .increment(1);
            }
            OutcomeKind::Failed => {
                metrics::counter!("task.failed", "metadata_type" => metadata_type)
                    .increment(1);
            }
            OutcomeKind::Retried => {
                metrics::counter!("task.retried", "metadata_type" => metadata_type)
                    .increment(1);
            }
        }
    }
}
