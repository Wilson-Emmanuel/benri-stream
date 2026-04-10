use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tracing::Instrument;

use domain::ports::task::{TaskConsumer, TaskRepository};
use domain::task::result::OutcomeKind;
use domain::task::trace_context::with_trace_id;
use domain::task::{Task, TaskId, TaskStatus};

use crate::handlers::TaskHandlerInvoker;

const EMPTY_POLL_DELAY: Duration = Duration::from_secs(1);
const POP_ERROR_DELAY: Duration = Duration::from_secs(5);

pub struct TaskConsumerLoop {
    consumer: Arc<dyn TaskConsumer>,
    task_repo: Arc<dyn TaskRepository>,
    handler: Arc<dyn TaskHandlerInvoker>,
    /// Bounds in-flight tasks. A permit is acquired before each pop and held
    /// for the task's lifetime, so the loop backpressures naturally.
    concurrency: Arc<Semaphore>,
}

impl TaskConsumerLoop {
    pub fn new(
        consumer: Arc<dyn TaskConsumer>,
        task_repo: Arc<dyn TaskRepository>,
        handler: Arc<dyn TaskHandlerInvoker>,
        max_concurrent_tasks: usize,
    ) -> Self {
        Self {
            consumer,
            task_repo,
            handler,
            concurrency: Arc::new(Semaphore::new(max_concurrent_tasks)),
        }
    }

    /// Consumes tasks until `shutdown` is set to `true`, then drains
    /// in-flight handlers before returning.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let mut in_flight: JoinSet<()> = JoinSet::new();

        loop {
            if *shutdown.borrow() {
                tracing::info!(
                    in_flight = in_flight.len(),
                    "task consumer shutting down; waiting for in-flight tasks to drain",
                );
                while in_flight.join_next().await.is_some() {}
                return;
            }

            // Acquire a permit before popping so we never dequeue a task we
            // can't immediately run. The permit is moved into the spawned future
            // and dropped when it completes.
            let permit = tokio::select! {
                _ = shutdown.changed() => continue,
                res = self.concurrency.clone().acquire_owned() => res,
            };
            let permit = match permit {
                Ok(p) => p,
                // Semaphore closed — unreachable in normal operation.
                Err(_) => return,
            };

            let pop_result = tokio::select! {
                _ = shutdown.changed() => continue,
                r = self.consumer.pop() => r,
            };

            match pop_result {
                Ok(Some(task_id)) => {
                    let task_repo = self.task_repo.clone();
                    let handler = self.handler.clone();
                    in_flight.spawn(async move {
                        Self::process_one(&task_repo, &handler, &task_id).await;
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    tokio::time::sleep(EMPTY_POLL_DELAY).await;
                }
                Err(e) => {
                    drop(permit);
                    tracing::error!(error = %e, "failed to pop from queue");
                    tokio::time::sleep(POP_ERROR_DELAY).await;
                }
            }

            // Drain completed futures so the JoinSet doesn't grow unbounded.
            while in_flight.try_join_next().is_some() {}
        }
    }

    /// Runs a single task through the handler dispatcher and persists the
    /// outcome. Takes no `&self` so it can be called from a detached future.
    async fn process_one(
        task_repo: &Arc<dyn TaskRepository>,
        handler: &Arc<dyn TaskHandlerInvoker>,
        task_id: &TaskId,
    ) {
        let task = match Self::fetch_in_progress_task(task_repo, task_id).await {
            Some(t) => t,
            None => return,
        };

        let outcome = Self::dispatch_with_trace(handler, &task).await;

        let metadata_type = task.metadata_type.clone();
        let outcome_kind = outcome.kind;

        if let Err(e) = task_repo.batch_update(&[outcome.update]).await {
            tracing::error!(
                task_id = %task.id,
                error = %e,
                "failed to persist task result — stale recovery will reset",
            );
        }

        Self::record_metric(outcome_kind, metadata_type);
    }

    /// Fetches a task and returns it only when it is `InProgress`. Logs and
    /// returns `None` for any other outcome so the caller can short-circuit.
    async fn fetch_in_progress_task(
        task_repo: &Arc<dyn TaskRepository>,
        task_id: &TaskId,
    ) -> Option<Arc<Task>> {
        let task = match task_repo.find_by_id(task_id).await {
            Ok(Some(t)) => Arc::new(t),
            Ok(None) => {
                tracing::warn!(task_id = %task_id, "task not found in DB, skipping");
                return None;
            }
            Err(e) => {
                tracing::error!(task_id = %task_id, error = %e, "failed to fetch task");
                return None;
            }
        };

        if task.status != TaskStatus::InProgress {
            tracing::debug!(
                task_id = %task_id,
                status = ?task.status,
                "task not IN_PROGRESS, skipping (already processed or reset)",
            );
            return None;
        }

        Some(task)
    }

    /// Dispatches a task inside a tracing span and a `with_trace_id` scope so
    /// any tasks the handler schedules inherit this task's trace id.
    async fn dispatch_with_trace(
        handler: &Arc<dyn TaskHandlerInvoker>,
        task: &Arc<Task>,
    ) -> domain::task::TaskRunOutcome {
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

        let handler = handler.clone();
        let task_for_dispatch = task.clone();
        with_trace_id(
            task.trace_id.clone(),
            async move { handler.dispatch(&task_for_dispatch).await }.instrument(span),
        )
        .await
    }

    /// Increments the appropriate task counter metric.
    fn record_metric(kind: OutcomeKind, metadata_type: String) {
        // OutcomeKind is unambiguous for metrics: TaskStatus::Pending covers
        // both retries and successful recurring reschedules, but OutcomeKind
        // distinguishes them.
        match kind {
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
