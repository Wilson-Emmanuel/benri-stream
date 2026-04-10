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
    /// Limits how many tasks this worker can be running at once. The
    /// consumer acquires a permit before popping the next task and
    /// releases it when the task finishes, so the pop loop naturally
    /// backpressures against the concurrency cap.
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

    /// Consume tasks until `shutdown` is set to `true`. On shutdown, any
    /// in-flight tasks are allowed to finish (bounded by their
    /// processing timeouts). No new tasks are popped.
    ///
    /// Concurrency shape: the loop holds up to `max_concurrent_tasks`
    /// in-flight handler runs via a `Semaphore`, acquiring a permit
    /// *before* popping from the queue so we never take a task off
    /// Redis we don't have capacity to run. Each permit is held for
    /// the lifetime of the spawned per-task future — the semaphore's
    /// internal counter backpressures the pop loop automatically.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        // Track in-flight per-task futures so shutdown can drain them.
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

            // Block the pop loop until we have capacity to actually
            // run whatever we pop. Attaching the permit to the spawned
            // task's lifetime (via `OwnedSemaphorePermit`) keeps the
            // accounting automatic — when the task future finishes, the
            // permit drops and a new slot opens up.
            let permit = tokio::select! {
                _ = shutdown.changed() => continue,
                res = self.concurrency.clone().acquire_owned() => {
                    match res {
                        Ok(p) => p,
                        Err(_) => {
                            // Semaphore closed — shouldn't happen.
                            return;
                        }
                    }
                }
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

            // Reap any finished in-flight tasks opportunistically so
            // the JoinSet doesn't grow unbounded. `try_join_next` is
            // non-blocking and just drains whatever is already done.
            while in_flight.try_join_next().is_some() {}
        }
    }

    /// Run a single task through the handler dispatcher and persist
    /// the outcome. No `&self` so it can be called from a detached
    /// tokio task without a 'static bound on `Self`.
    async fn process_one(
        task_repo: &Arc<dyn TaskRepository>,
        handler: &Arc<dyn TaskHandlerInvoker>,
        task_id: &TaskId,
    ) {
        let task: Arc<Task> = match task_repo.find_by_id(task_id).await {
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
        //
        // Wrap the dispatch in a `with_trace_id` scope so any sub-tasks
        // the handler schedules (e.g. a follow-up job from
        // `process_video`) inherit this task's trace_id via the
        // scheduler's ambient read. Without this, only the top-level
        // request's tasks would carry the id and the chain would break
        // at every task boundary.
        let handler = handler.clone();
        let task_for_dispatch = task.clone();
        let trace_id = task.trace_id.clone();
        let outcome = with_trace_id(
            trace_id,
            async move { handler.dispatch(&task_for_dispatch).await }.instrument(span),
        )
        .await;

        let metadata_type = task.metadata_type.clone();
        let outcome_kind = outcome.kind;

        if let Err(e) = task_repo.batch_update(&[outcome.update]).await {
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
