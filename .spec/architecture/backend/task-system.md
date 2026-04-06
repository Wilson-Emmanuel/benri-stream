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
    pub metadata: serde_json::Value,  // task-specific payload
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

### TaskMetadata trait

Every task type implements this. The struct name is the routing key.

```rust
pub trait TaskMetadata: Send + Sync + Serialize + DeserializeOwned {
    /// Max time before the worker cancels processing. Each type sets its own.
    fn processing_timeout(&self) -> Duration { Duration::from_secs(300) }

    /// If set, the task is recurring — rescheduled after each completion.
    fn execution_interval(&self) -> Option<Duration> { None }

    /// Max retries on transient failure. None = no retries, straight to dead letter.
    fn max_retries(&self) -> Option<i32> { None }

    /// Base delay for exponential backoff. Actual = base * 2^attempt_count, capped at 30min.
    fn retry_base_delay(&self) -> Duration { Duration::from_secs(30) }

    /// Tasks with the same ordering key are processed sequentially.
    fn ordering_key(&self) -> Option<String> { None }

    /// System tasks are recurring and auto-recreated if missing.
    fn is_system_task(&self) -> bool { false }
}
```

**Where to define metadata**:
- Scheduled by use cases (application layer) → define in `crates/domain/src/task/`
- Scheduled only by worker internals (system tasks) → define alongside the handler
  in `crates/worker/src/handlers/`

### TaskResult

Returned by every handler. Controls the state transition.

```rust
pub enum TaskResult {
    /// Completed. Recurring tasks reschedule after execution_interval.
    Success { message: Option<String> },

    /// Transient failure — retry with backoff if retries remain, else dead letter.
    RetryableFailure { error: String },

    /// Permanent failure — dead letter immediately.
    PermanentFailure { error: String },

    /// Skip — preconditions not met. One-time → completed. Recurring → reschedule.
    Skip { reason: String },
}
```

### Ports

```rust
// In crates/domain/src/task/ports.rs

pub trait TaskRepository: Send + Sync {
    async fn create(&self, task: &Task) -> Result<Task, RepositoryError>;
    async fn find_by_id(&self, id: &TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn find_by_ids(&self, ids: &[TaskId]) -> Result<Vec<Task>, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    async fn batch_update(&self, updates: &[TaskUpdate]) -> Result<(), RepositoryError>;
    async fn reset_stale(&self, threshold: DateTime<Utc>) -> Result<i32, RepositoryError>;
    async fn count_active_by_type(&self, metadata_type: &str) -> Result<i64, RepositoryError>;
    async fn exists_active_by_ordering_key(&self, metadata_type: &str, key: &str) -> Result<bool, RepositoryError>;
}

pub trait TaskPublisher: Send + Sync {
    /// Publish task IDs to the queue.
    async fn publish(&self, task_ids: &[TaskId]) -> Result<bool, QueueError>;
}

pub trait TaskConsumer: Send + Sync {
    /// Pop the next task ID from the queue. Returns None if the queue is empty.
    async fn pop(&self) -> Result<Option<TaskId>, QueueError>;
}

pub trait TaskHandlerInvoker: Send + Sync {
    async fn invoke(&self, task: &Task) -> TaskResult;
}
```

### TaskScheduler (domain service)

Single entry point for creating tasks. Use cases call this — never call
`TaskRepository::create` directly.

```rust
// In crates/domain/src/task/scheduler.rs
impl TaskScheduler {
    pub async fn schedule<M: TaskMetadata>(
        &self,
        metadata: &M,
        trace_id: Option<String>,
    ) -> Result<Task, RepositoryError> {
        // serializes metadata, captures trace_id, creates Task with PENDING status
    }
}
```

**Ordering-key dedup**: when the metadata declares an `ordering_key`, `schedule()`
is **dedup-by-default** within `(metadata_type, ordering_key)`. If an active
(`PENDING` or `IN_PROGRESS`) task already exists for the same pair, `schedule()`
returns that existing task instead of creating a duplicate. This is enforced via
`TaskRepository::exists_active_by_ordering_key`. Callers do not need to check
themselves — repeated `schedule()` calls for the same key are safe and idempotent.

This means `ordering_key` carries two meanings:
1. **Sequential processing** — tasks with the same key never run concurrently.
2. **Active uniqueness** — at most one active task per `(metadata_type, key)`.

Callers that need ordering without dedup, or dedup without sequential processing,
should not be added without revisiting this design.

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
3. Skip if not IN_PROGRESS (already processed)
4. Dispatch to handler by metadata type (with per-task timeout)
5. Compute update via `task.compute_update(result)`
6. Write result to DB

### Handler Dispatch

Explicit map — no reflection, no scanning. Adding a handler = one new entry.

```rust
// crates/worker/src/handlers/mod.rs
fn build_handler_map() -> HashMap<String, Box<dyn TaskHandler>> {
    let mut map = HashMap::new();
    map.insert("ProcessVideoTaskMetadata".into(), Box::new(ProcessVideoHandler::new(/* deps */)));
    map.insert("CleanupStaleVideosTaskMetadata".into(), Box::new(CleanupStaleVideosHandler::new(/* deps */)));
    map
}
```

Each handler is a struct with a `handle` method returning `TaskResult`. The handler
calls the corresponding use case, maps its Result to a TaskResult.

### Outbox Poller

Runs periodically. Acquires distributed lock → polls PENDING tasks in batch → marks
IN_PROGRESS in DB → publishes to queue. Lock ensures only one instance polls at a time.

### Stale Recovery

Runs periodically. Acquires distributed lock → finds IN_PROGRESS tasks older than
`processing_timeout + buffer` → resets to PENDING.

### System Task Checker

Runs periodically. For each registered system task metadata, checks if an active
(PENDING or IN_PROGRESS) task exists. If not, creates one. Uses distributed lock.

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
