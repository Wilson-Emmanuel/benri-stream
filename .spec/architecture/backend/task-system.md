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

    // Denormalized scheduling config — see below.
    pub max_retries: Option<i32>,
    pub retry_base_delay_ms: i64,
    pub execution_interval_ms: Option<i64>,
    pub processing_timeout_ms: i64,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum TaskStatus { Pending, InProgress, Completed, DeadLetter }
```

**Why scheduling config is denormalized onto the row**: the `metadata` payload
is stored as `serde_json::Value`, which loses the concrete `TaskMetadata` type.
If the consumer had to call `metadata.max_retries()` at runtime to decide
whether to dead-letter, it would need a type registry to deserialize the Value
back into a typed metadata — extra moving parts and risk of drift. Instead,
`TaskScheduler::schedule` reads the scheduling config from the trait at
schedule time and writes it into the four dedicated columns. The consumer and
`Task::compute_update` read the config directly from the row, and changes to
the trait defaults only affect newly-scheduled tasks (values at the time of
schedule are latched).

### TaskMetadata trait

Every task type implements this. Each type should declare a
`pub const METADATA_TYPE: &'static str` equal to its struct name and return
that const from `metadata_type_name()`, so the handler dispatch key and the
trait impl cannot drift.

```rust
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time before the worker cancels processing.
    fn processing_timeout(&self) -> Duration { Duration::from_secs(300) }

    /// If set, the task is recurring — rescheduled after each completion.
    fn execution_interval(&self) -> Option<Duration> { None }

    /// Max retries on transient failure. None = no retries, straight to dead letter.
    fn max_retries(&self) -> Option<i32> { None }

    /// Base delay for exponential backoff. Actual = base * 2^attempt_count, capped at 30min.
    fn retry_base_delay(&self) -> Duration { Duration::from_secs(30) }

    /// Tasks with the same ordering key are processed sequentially and
    /// dedup-by-default on schedule.
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

### Ports

```rust
// In crates/domain/src/task/ports.rs

/// Worker-internal task lifecycle operations. Task creation from use cases
/// goes through TaskScheduler + TaskMutations inside a TxScope.
pub trait TaskRepository: Send + Sync {
    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError>;
    async fn reset_stale(&self, threshold: DateTime<Utc>) -> Result<i32, RepositoryError>;
    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError>;
}

/// Task operations performed inside a TxScope (see ports/unit_of_work.rs).
/// Task creation and the dedup-lookup both live here so they can share
/// a transaction with the triggering business mutation.
pub trait TaskMutations: Send {
    async fn create(&mut self, task: &Task) -> Result<Task, RepositoryError>;
    async fn find_active_by_ordering_key(
        &mut self,
        metadata_type: &str,
        ordering_key: &str,
    ) -> Result<Option<Task>, RepositoryError>;
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
        trace_id: Option<String>,
        run_at: Option<DateTime<Utc>>,
    ) -> Result<Task, RepositoryError> {
        // 1. If metadata has an ordering_key, check for an active
        //    (PENDING or IN_PROGRESS) task with the same
        //    (metadata_type, ordering_key) — return it if found (dedup).
        // 2. Otherwise, read scheduling config from the metadata trait
        //    and persist it into the new Task row.
    }
}
```

**Ordering-key dedup**: when the metadata declares an `ordering_key`, `schedule()`
is **dedup-by-default** within `(metadata_type, ordering_key)`. If an active
(`PENDING` or `IN_PROGRESS`) task already exists for the same pair, `schedule()`
returns that existing task instead of creating a duplicate. This is enforced via
`TaskMutations::find_active_by_ordering_key`. Callers do not need to check
themselves — repeated `schedule()` calls for the same key are safe and idempotent.
A partial unique index on `tasks(metadata_type, ordering_key) WHERE status IN
('PENDING','IN_PROGRESS')` backs up the in-tx check against races.

This means `ordering_key` carries two meanings:
1. **Sequential processing** — tasks with the same key never run concurrently
   (enforced by `find_pending` via a CTE with `DISTINCT ON (ordering_key)` and
   a blocked-keys filter against IN_PROGRESS siblings).
2. **Active uniqueness** — at most one active task per `(metadata_type, key)`.

Callers that need ordering without dedup, or dedup without sequential processing,
should not be added without revisiting this design.

**`run_at`**: pass `Some(future_timestamp)` to defer the task's first eligibility.
Defaults to `now`.

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

Runs periodically. Acquires distributed lock → finds IN_PROGRESS tasks older
than a threshold → resets to PENDING. Observes shutdown signal.

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

`DistributedLock::acquire` returns a `LockToken` on success. Release is
ownership-checked via a Lua script: `release(key, token)` only deletes the
key if its current value matches the token. This prevents a caller whose
lock TTL has already expired from releasing another worker's lock.

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
