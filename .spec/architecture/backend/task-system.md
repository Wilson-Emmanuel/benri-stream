# Task System

Durable, distributed background job engine built on an outbox pattern + message queue.
Follows hexagonal architecture: domain defines contracts as traits, infrastructure
and worker implement them.

---

## How It Works

```
Use case (application)
    │ creates Task (PENDING) in DB — same transaction as the business write
    ▼
Outbox Poller (worker, with distributed lock)
    │ polls PENDING tasks in batch
    │ marks IN_PROGRESS in DB first (update-first — prevents duplicates)
    │ publishes to message queue (e.g. Redis List)
    ▼
Message queue — FIFO
    ▼
Task Consumer (worker, pop loop)
    │ pops one task ID
    │ fetches full task from DB
    │ skips if not IN_PROGRESS (already processed)
    │ dispatches to handler by metadata type
    │ writes result to DB
    ▼
Handler → returns TaskResult → DB updated to COMPLETED | DEAD_LETTER | PENDING (retry/reschedule)

Stale Recovery (worker)
    │ resets stuck IN_PROGRESS tasks → PENDING
```

**Key properties**:
- **DB is source of truth** — the queue is ephemeral. If the queue loses data, the poller
  re-publishes PENDING tasks from DB.
- **At-least-once delivery** — handlers must be idempotent.
- **Update-first publishing** — tasks are marked IN_PROGRESS *before* publishing to
  the queue. If the instance dies after the DB update but before the publish, stale
  recovery resets them.
- **Swappable** — the queue is behind `TaskPublisher` / `TaskConsumer` traits. The
  broker (e.g. Redis List, Kafka, SQS) can be swapped by changing only the
  infrastructure implementation.

---

## Domain Types

All in `crates/domain/src/task/`.

### Task

```rust
pub struct Task {
    pub id: TaskId,
    pub metadata_type: String,       // routing key — maps to handler
    pub metadata: serde_json::Value,  // task-specific payload (typed by metadata_type)
    pub status: TaskStatus,
    pub ordering_key: Option<String>,
    pub trace_id: Option<String>,    // W3C traceId from the request that created this task
    pub attempt_count: i32,
    pub next_run_at: DateTime<Utc>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum TaskStatus { Pending, InProgress, Completed, DeadLetter }
```

**Scheduling config is NOT stored on the row.** `max_retries`,
`retry_base_delay`, `execution_interval`, and `processing_timeout` live on
the typed `TaskMetadata` impl and are read at run time. The
`HandlerAdapter` deserializes the metadata from `task.metadata: Value`
into the concrete type before computing the update, then passes the typed
metadata to `Task::compute_update<M: TaskMetadata>`.

This design has two consequences:

- **Config changes are immediate.** Bumping `max_retries` from 5 to 10 in
  the metadata trait impl applies to existing tasks on their next run.
  No migration needed.
- **Rolling deploys may briefly run on different config.** During a rolling
  upgrade, two workers with different code versions may use different
  values for the same task. Acceptable at our scale.

### TaskMetadata trait

Every task type implements this. Each type should declare a
`pub const METADATA_TYPE: &'static str` equal to its struct name and return
that const from `metadata_type_name()`, so the handler dispatch key and the
trait impl cannot drift.

```rust
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time before the worker cancels processing. MUST stay below
    /// 30 minutes (the global stale-recovery threshold is 1 hour, so a
    /// 30-minute task plus retry delay still fits inside the safety
    /// window). See "Stale Recovery" below.
    fn processing_timeout(&self) -> Duration { Duration::from_secs(300) }

    /// If set, the task is recurring — rescheduled after each completion.
    fn execution_interval(&self) -> Option<Duration> { None }

    /// Max retries on transient failure. None = no retries, straight to dead letter.
    fn max_retries(&self) -> Option<i32> { None }

    /// Base delay for exponential backoff. Actual = base * 2^attempt_count, capped at 30min.
    fn retry_base_delay(&self) -> Duration { Duration::from_secs(30) }

    /// Tasks with the same ordering key are processed sequentially —
    /// at most one active at a time per (metadata_type, ordering_key),
    /// enforced by `find_pending`'s CTE which filters out keys with an
    /// IN_PROGRESS sibling. Used for resource serialization (e.g.
    /// "only one task at a time can mutate this video"), NOT for
    /// deduplication. Multiple `schedule()` calls for the same logical
    /// task are allowed and create multiple rows. Handlers MUST be
    /// idempotent (see "Handler Idempotency" below).
    fn ordering_key(&self) -> Option<String> { None }

    /// System tasks are recurring and auto-recreated if missing.
    fn is_system_task(&self) -> bool { false }

    /// Routing key — must equal the struct's METADATA_TYPE const.
    fn metadata_type_name(&self) -> &'static str;
}
```

**Where to define metadata**:
- Scheduled by use cases → `crates/domain/src/task/metadata/`
- Scheduled only by worker internals → alongside the handler in `crates/worker/src/handlers/`

For the authoritative list of task types and their config values, see
[`business-spec/task-system/task-catalog.md`](../../business-spec/task-system/task-catalog.md).

### TaskResult

Returned by every handler. Controls the state transition computed by
`Task::compute_update`.

```rust
pub enum TaskResult {
    /// Completed. Recurring tasks (metadata.execution_interval is set)
    /// reschedule for the next interval; one-shot tasks → COMPLETED.
    /// `reschedule_after` overrides the configured interval.
    Success {
        message: Option<String>,
        reschedule_after: Option<Duration>,
    },

    /// Transient failure — retry with backoff if retries remain, else
    /// dead letter. `retry_after` overrides the calculated backoff delay.
    RetryableFailure {
        error: String,
        retry_after: Option<Duration>,
    },

    /// Permanent failure — dead letter immediately.
    PermanentFailure { error: String },

    /// Skip — preconditions not met. One-shot → COMPLETED. Recurring →
    /// reschedule without counting as a failure.
    Skip { reason: String },

    /// Terminate a recurring task — mark COMPLETED and do not reschedule.
    Terminate { reason: String },
}
```

**Skip / Terminate reason storage**: when computing the TaskUpdate, the
`reason` from `Skip` is stored in the `error` column prefixed with
`"Skipped: "`, and `Terminate` is prefixed with `"Terminated: "`. The
`error` column doubles as a "last message" channel since the schema has
no dedicated last-message column. Any query for actual failures should
filter out rows whose error starts with `"Skipped: "` / `"Terminated: "`.

### Ports

```rust
// In crates/domain/src/task/ports.rs

/// Worker-internal task lifecycle operations + bulk creation. Single-task
/// creation from use cases goes through TaskScheduler + TaskMutations
/// inside a TxScope. Bulk creation is pool-backed since one INSERT
/// statement is atomic and has nothing to bundle with.
pub trait TaskRepository: Send + Sync {
    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError>;
    async fn reset_stale(&self) -> Result<i32, RepositoryError>;
    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError>;
    async fn bulk_create(&self, tasks: &[Task]) -> Result<(), RepositoryError>;
}

/// Single-task creation inside a TxScope (see ports/unit_of_work.rs).
pub trait TaskMutations: Send {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError>;
}

pub trait TaskPublisher: Send + Sync {
    /// Publish task IDs to the queue.
    async fn publish(&self, task_ids: &[TaskId]) -> Result<bool, QueueError>;
}

pub trait TaskConsumer: Send + Sync {
    /// Pop the next task ID from the queue. Returns None if the queue is empty.
    async fn pop(&self) -> Result<Option<TaskId>, QueueError>;
}

// Handler dispatch (worker layer): see TypedTaskHandler / ErasedHandler below.
```

### TaskScheduler (stateless domain utility)

Single entry point for creating tasks. Use cases call this inside a `TxScope`
opened via `UnitOfWork::begin` — never call `TaskMutations::create` directly.

```rust
// In crates/domain/src/task/scheduler.rs
impl TaskScheduler {
    pub async fn schedule<M: TaskMetadata>(
        tasks: &mut dyn TaskMutations,
        metadata: &M,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        // Construct a Task with PENDING status and insert via tasks.create.
        // No deduplication — handlers must be idempotent.
    }
}
```

**No deduplication.** Repeated `schedule()` calls for the same logical
task create multiple rows. The system relies on **handler idempotency**
and `ordering_key`-based sequential processing to handle duplicates safely:
the second runner sees the work already done (by the first) and returns
`Skip` or a no-op `Success`. See "Handler Idempotency" below.

Why no dedup? Two reasons:
1. **Conceptual clarity.** `ordering_key` is for resource serialization
   ("only one task at a time can mutate this resource"), not for "is this
   task already scheduled". The catalog and handler code are simpler when
   the two concerns are not conflated.
2. **No racy code paths.** A dedup check requires a read inside the tx
   plus a backstop unique index. The check has known race windows that
   convert into unique-violation errors at the index. Since handlers are
   idempotent anyway, the dedup check adds complexity for no safety
   benefit.

**`run_at`**: pass `Some(future_timestamp)` to defer the task's first
eligibility. Defaults to `now`.

**`trace_id`**: not currently propagated. Tasks are created with
`trace_id: None`. When OpenTelemetry / a tracing-context port is wired,
populate it from the current span inside `TaskScheduler::schedule`. The
column exists on the row and the consumer reads it for span propagation.

---

## Infrastructure

### Queue (e.g. Redis List)

In `crates/infrastructure/src/redis/`.

| File | Role |
|------|------|
| `task_publisher.rs` | Implements `TaskPublisher` — pushes task IDs to the queue |
| `task_consumer.rs` | Implements `TaskConsumer` — pops task IDs from the queue |
| `distributed_lock.rs` | Acquire/release lock with TTL for poller, recovery, system checker |

The queue implementation can be swapped (Redis, Kafka, SQS, etc.) by providing new
implementations of `TaskPublisher` and `TaskConsumer` — no changes to domain or worker.

### Postgres

In `crates/infrastructure/src/postgres/task_repository.rs`.

Implements `TaskRepository`. Key queries:
- `find_pending` — respects ordering keys (skips tasks whose key has an IN_PROGRESS sibling)
- `mark_in_progress` — batch update, sets `started_at`
- `reset_stale` — finds IN_PROGRESS tasks older than threshold, resets to PENDING

---

## Worker

In `crates/worker/src/`.

### Task Consumer Loop

Pop loop consuming from the queue. For each task:
1. Pop a task ID from the queue via `TaskConsumer::pop()`
2. Fetch the full task from DB
3. Skip if not `IN_PROGRESS` (already processed or reset)
4. Open a tracing span with `task_id`, `metadata_type`, `attempt_count`, and
   `trace_id` (propagated from the request that scheduled the task)
5. Dispatch to handler, wrapped in `tokio::time::timeout(task.processing_timeout())`.
   On timeout, the handler future is dropped and the result becomes
   `RetryableFailure("handler timed out after ...")`
6. Compute update via `task.compute_update(result)`
7. Write result to DB

The consumer loop observes a `watch::Receiver<bool>` shutdown signal and
drains gracefully on SIGINT/SIGTERM — no new pops after shutdown, but
in-flight tasks are allowed to finish (bounded by their timeouts).

### Handler Dispatch — typed via `TypedTaskHandler`

Handlers receive a **typed metadata reference**, not a raw JSON value. Each
handler declares its metadata type via an associated type; a thin adapter
deserializes the task's JSON payload into the concrete type before invoking
the handler.

```rust
// crates/worker/src/handlers/mod.rs

#[async_trait]
pub trait TypedTaskHandler: Send + Sync {
    type Metadata: TaskMetadata + Send + Sync + 'static;
    async fn handle(
        &self,
        metadata: &Self::Metadata,
        ctx: &TaskExecutionContext,
    ) -> TaskResult;
}

#[async_trait]
pub trait ErasedHandler: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskResult;
}

pub struct HandlerAdapter<H: TypedTaskHandler + 'static> { ... }

impl<H: TypedTaskHandler + 'static> ErasedHandler for HandlerAdapter<H> {
    async fn invoke(&self, task: &Task) -> TaskResult {
        let metadata: H::Metadata = serde_json::from_value(task.metadata.clone())?;
        self.inner.handle(&metadata, &ctx).await
    }
}
```

The dispatch map is explicit — no reflection, no scanning. Adding a handler =
one new entry. Use the metadata struct's `METADATA_TYPE` const as the key so
the registration cannot drift from the metadata's own `metadata_type_name()`:

```rust
let mut map: HashMap<String, Arc<dyn ErasedHandler>> = HashMap::new();
map.insert(
    ProcessVideoTaskMetadata::METADATA_TYPE.to_string(),
    HandlerAdapter::wrap(Arc::new(ProcessVideoHandler::new(uc))),
);
```

### Outbox Poller

Runs periodically. Acquires distributed lock → polls PENDING tasks in batch
(respecting ordering keys) → marks IN_PROGRESS in DB → publishes to queue.
Lock ensures only one instance polls at a time. Observes shutdown signal.

### Stale Recovery

Runs periodically. Acquires distributed lock → finds IN_PROGRESS tasks
whose `started_at` is older than the **global stale threshold** → resets
to PENDING. Observes shutdown signal.

The threshold is a **fixed 1 hour**, enforced inside
`TaskRepository::reset_stale`. The reasoning:

- The threshold must exceed every task type's `processing_timeout`,
  otherwise stale recovery would reset a still-running handler and
  another worker would pick the task up — double processing.
- Per-task thresholds (computed from `started_at + processing_timeout`)
  would work but require persisting `processing_timeout` on the row,
  which conflicts with our "config lives in metadata trait" model.
- Picking a single value larger than any task's timeout is simple and
  documented.

**Hard limit on `processing_timeout`**: 30 minutes per task type. The
limit gives a 30-minute safety buffer below the 1-hour threshold to
absorb retry delays, scheduling jitter, and clock skew. New task types
with longer-running handlers would require revisiting the threshold
(and updating this doc + the trait docstring + the worker constant).

### System Task Checker

Runs periodically. For each registered system task metadata, checks if an
active (PENDING or IN_PROGRESS) task exists via `count_active_by_type`. If
not, schedules one via `TaskScheduler::schedule` inside a `uow.begin` tx.
Uses distributed lock. Observes shutdown signal.

Note that a working recurring task normally reschedules *itself* on success
via `compute_update` (which reads `execution_interval_ms` from the row). The
system task checker is a bootstrap / safety net — it only creates new tasks
when no active instance exists, e.g. after cold start, after a dead letter,
or if a bug caused the rescheduling to be lost.

### Distributed Lock

Defined as `DistributedLockPort` in `domain/ports/distributed_lock.rs`.
Worker components hold `Arc<dyn DistributedLockPort>`, not a concrete
type, so the implementation is swappable.

`acquire` returns `Option<LockToken>`. Release is ownership-checked: a
release with a stale token is a no-op, not a delete of the current
holder's lock. The Redis impl (`infrastructure::redis::distributed_lock::
RedisDistributedLock`) uses `SET NX EX` for acquire and a Lua check-and-
delete script for release.

---

## Worker Tuning

**Concurrency limit** — configurable max concurrent tasks per worker. Depends on instance
capacity (CPU, RAM, GPU).

**Processing timeout** — defined per task metadata type via `processing_timeout()`.
If exceeded, the worker cancels the task and stale recovery resets it to PENDING.

**Task starvation** — when workers are saturated with long-running tasks, short tasks
pile up. Two strategies:
- **Worker groups** — separate pools for long-running vs short/priority tasks, consuming
  from different queues or filtering by metadata type
- **Per-type concurrency limits** — cap how many slots a single task type can occupy
  per worker

Which strategy depends on the task mix. The architecture supports both.

---

## Handler Idempotency

At-least-once delivery means a handler may run twice for the same task. Most handlers
are naturally idempotent:

| Pattern | Why safe | Examples |
|---------|----------|----------|
| Upsert | Second run overwrites with same data | Index/update records |
| Delete | Deleting already-deleted is a no-op | Cleanup operations |
| Conditional write | Second run sees write already happened | Status transitions with guards |

The only duplicate window: handler succeeds → worker crashes before DB write →
stale recovery resets → handler runs again. This is inherent to at-least-once systems.

---

## Execution Flow

```
Use case → TaskScheduler.schedule() [inside DB transaction]
    ↓
Outbox Poller [lock → poll PENDING → mark IN_PROGRESS → publish to queue]
    ↓
Message queue [FIFO] (e.g. Redis List)
    ↓
Consumer [pop → fetch from DB → skip if processed → dispatch to handler → write result]
    ↓
Handler → TaskResult → compute_update() → DB: COMPLETED | DEAD_LETTER | PENDING (retry/reschedule)

Stale Recovery [periodic → reset stuck IN_PROGRESS → PENDING]
System Task Checker [periodic → recreate missing system tasks]
```

---

## Adding a New Task Type

1. **Define metadata struct** — implements `TaskMetadata`. Place in `domain/src/task/`
   if scheduled by use cases, or alongside the handler in `worker/src/handlers/` if
   worker-only.
2. **Write the use case** — business logic in `application`. Returns `Result`.
3. **Write the handler** — in `worker/src/handlers/`. Calls use case, maps Result to
   TaskResult. Thin adapter — no business logic.
4. **Register in handler map** — add one entry in `worker/src/handlers/mod.rs`.
5. **If system task** — add to the system task checker's list.
6. **If scheduled from a use case** — call `task_scheduler.schedule()` inside the
   transaction.
7. **Document in `business-spec/task-system/task-catalog.md`**.

Both the handler map and system task list are explicit — no auto-discovery. Forgetting
to register results in `PermanentFailure("No handler for ...")`.

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| Task entity, TaskStatus, TaskMetadata trait | `domain` | `src/task/mod.rs` |
| TaskResult | `domain` | `src/task/result.rs` |
| TaskScheduler | `domain` | `src/task/scheduler.rs` |
| TaskRepository, TaskPublisher traits | `domain` | `src/task/ports.rs` |
| Task metadata (scheduled by use cases) | `domain` | `src/task/metadata/` |
| PostgresTaskRepository | `infrastructure` | `src/postgres/task_repository.rs` |
| Queue publisher (e.g. Redis) | `infrastructure` | `src/redis/task_publisher.rs` |
| Queue consumer (e.g. Redis) | `infrastructure` | `src/redis/task_consumer.rs` |
| Distributed lock | `infrastructure` | `src/redis/distributed_lock.rs` |
| Task consumer loop | `worker` | `src/consumer.rs` |
| Outbox poller | `worker` | `src/poller.rs` |
| Stale recovery | `worker` | `src/recovery.rs` |
| System task checker | `worker` | `src/system_checker.rs` |
| Handler dispatch map | `worker` | `src/handlers/mod.rs` |
| Individual handlers | `worker` | `src/handlers/[name]_handler.rs` |
| Task metadata (worker-only) | `worker` | `src/handlers/` (alongside handler) |
