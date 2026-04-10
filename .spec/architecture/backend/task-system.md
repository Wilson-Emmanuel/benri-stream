# Task System

Durable background job engine built on an outbox pattern + message queue. Domain defines contracts as traits; infrastructure and worker implement them.

---

## Flow

```
Use case creates Task (PENDING) in DB, atomic with business write
    ↓
Outbox Poller (distributed lock → poll PENDING → mark IN_PROGRESS → publish to queue)
    ↓
Consumer (pop → fetch from DB → skip if not IN_PROGRESS → dispatch → write result)
    ↓
Handler → TaskResult → compute_update → DB: COMPLETED | DEAD_LETTER | PENDING (retry)

Stale Recovery: periodic reset of stuck IN_PROGRESS tasks → PENDING
System Task Checker: periodic, recreates missing recurring tasks
```

**Key properties**:
- DB is source of truth — queue is ephemeral; poller re-publishes from DB if queue loses data
- At-least-once delivery — handlers must be idempotent
- Update-first publishing — tasks marked IN_PROGRESS before queue publish; stale recovery handles crashes between DB write and publish

---

## Domain Types

All in `crates/domain/src/task/`.

**Task** — `id`, `metadata_type` (routing key), `metadata` (JSON payload), `status`, `ordering_key`, `trace_id`, `attempt_count`, `next_run_at`, error/timing fields.

**TaskStatus** — `Pending | InProgress | Completed | DeadLetter`

**TaskMetadata trait** — implemented by each task type. Scheduling config (`max_retries`, `retry_base_delay`, `processing_timeout`, `execution_interval`) lives on the trait impl, not stored on the row. Config changes apply immediately to existing tasks on their next run.

Each type declares `pub const METADATA_TYPE: &str` and returns it from `metadata_type_name()` so the dispatch key cannot drift.

**TaskResult** — returned by every handler:

| Variant | Behavior |
|---------|----------|
| `Success` | Recurring tasks reschedule; one-shot → COMPLETED |
| `RetryableFailure` | Retry with backoff if retries remain, else dead letter |
| `PermanentFailure` | Dead letter immediately |
| `Skip` | Preconditions not met; one-shot → COMPLETED, recurring → reschedule |
| `Terminate` | Mark COMPLETED, do not reschedule |

**TaskScheduler** — stateless utility, single entry point for creating tasks. Two methods:
- `schedule_in_tx(tasks_mut, metadata, run_at)` — inside an open transaction, atomic with business writes
- `schedule_standalone(repo, metadata, run_at)` — pool-backed, no transaction needed

Reads trace_id from the ambient `trace_context::current_trace_id()` (tokio task-local set by API middleware).

No deduplication. Repeated schedules create multiple rows. Handlers are idempotent; the second runner sees work already done and returns `Skip`.

---

## Ports

| Trait | Location | Purpose |
|-------|----------|---------|
| `TaskRepository` | `domain/src/ports/task.rs` | Pool-backed CRUD, `find_pending`, `mark_in_progress`, `reset_stale`, `batch_update` |
| `TaskMutations` | `domain/src/ports/transaction.rs` | Single-task create inside open transaction |
| `TaskPublisher` | `domain/src/ports/task.rs` | Publish task IDs to queue |
| `TaskConsumer` | `domain/src/ports/task.rs` | Pop next task ID from queue |

---

## Worker Components

**Consumer loop** — pops task IDs, fetches from DB, skips if not IN_PROGRESS, dispatches to handler with `tokio::time::timeout(processing_timeout)`, writes result. Drains gracefully on SIGINT/SIGTERM.

**Handler dispatch** — `TypedTaskHandler` trait with associated `Metadata` type. `HandlerAdapter` erases the type: deserializes JSON into the concrete metadata, then invokes the typed handler. Dispatch map is explicit — one `HashMap` entry per handler, keyed on `METADATA_TYPE`.

**Outbox poller** — acquires distributed lock, polls PENDING tasks in batch (respecting ordering keys), marks IN_PROGRESS, publishes to queue.

**Stale recovery** — acquires distributed lock, resets IN_PROGRESS tasks with `started_at` older than 1 hour to PENDING. The threshold exceeds every task type's `processing_timeout` (hard limit: 30 min).

**System task checker** — acquires distributed lock, checks each registered system task metadata for an active instance via `count_active_by_type`, schedules missing ones via `schedule_standalone`.

**Distributed lock** — `DistributedLockPort` trait in domain. Redis impl uses `SET NX EX` for acquire, Lua check-and-delete for release.

---

## Handler Idempotency

| Pattern | Why safe |
|---------|----------|
| Upsert | Second run overwrites with same data |
| Delete | Already-deleted is a no-op |
| Conditional write | Second run sees write already happened (status guard) |

The only duplicate window: handler succeeds, worker crashes before DB write, stale recovery resets, handler runs again.

---

## Adding a New Task Type

1. Define metadata struct implementing `TaskMetadata` — in `domain/src/task/metadata/` if scheduled by use cases, in `worker/src/handlers/` if worker-only
2. Write the use case in `application`
3. Write the handler in `worker/src/handlers/` — thin adapter calling the use case, maps `Result` to `TaskResult`
4. Register in handler map — one entry in `worker/src/handlers/mod.rs`
5. If system task — add to system task checker's list
6. If scheduled from a use case — call `TaskScheduler::schedule_in_tx()` inside the transaction
7. Document in `business-spec/task-system/task-catalog.md`

No auto-discovery. Forgetting to register results in `PermanentFailure("No handler for ...")`.
