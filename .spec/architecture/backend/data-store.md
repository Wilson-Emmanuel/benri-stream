# Data Store

PostgreSQL via [sqlx](https://github.com/launchbadge/sqlx). Single source of truth for all application state.

---

## Tables

| Table | Defined in |
|-------|------------|
| `videos` | [Video entity](../../business-spec/video/video.md) |
| `tasks` | [Task system](./task-system.md), [Task catalog](../../business-spec/task-system/task-catalog.md) |

Schema derives from the business spec. Columns map 1:1 to entity attributes. The `tasks` table additionally carries `trace_id` (VARCHAR(32)) linking each task to the originating API request.

---

## Transactions

Use cases that need multiple writes to be atomic use `TransactionPort::run`. The port takes a closure that receives a `TxScope` — a handle providing access to `VideoMutations` and `TaskMutations` within the same DB transaction. Commits on `Ok`, rolls back on `Err` or panic.

```
tx_port.run(|scope| {
    scope.videos().update_status_if(id, Uploaded, Processing)?;
    TaskScheduler::schedule_in_tx(scope.tasks(), &metadata, None)?;
    Ok(())
})
```

This is how `CompleteUpload` atomically transitions a video and schedules a `ProcessVideo` task — if either write fails, neither commits.

Single-statement writes (e.g. `insert`, `find_by_id`) go directly through the pool-backed repository traits without a transaction.

---

## Ports and Implementations

| Layer | Trait | Implementation |
|-------|-------|----------------|
| domain | `VideoRepository` | `PostgresVideoRepository` |
| domain | `TaskRepository` | `PostgresTaskRepository` |
| domain | `TransactionPort` | `PgTransactionPort` |
| domain | `VideoMutations` | `PgVideoMutations` (inside tx) |
| domain | `TaskMutations` | `PgTaskMutations` (inside tx) |

All port methods are async and return `Result`. Implementations use sqlx runtime queries (not compile-time `query!` macros, since those require a live DB at build time).

---

## Conditional Updates

Status transitions use conditional `UPDATE ... WHERE status = $expected` to prevent race conditions:

```sql
UPDATE videos SET status = 'PROCESSING'
WHERE id = $1 AND status = 'UPLOADED'
```

Returns 0 affected rows if another worker already claimed the video. The caller checks the row count and skips (idempotent).

---

## Migrations

SQL files in `migrations/`, managed by `sqlx-cli`, applied at startup by the API server.

---

## Connection Pooling

| Process | Max connections |
|---------|----------------|
| API | 10 |
| Worker | 5 |

Configured via `PgPoolOptions` at startup. Kept low to avoid exhausting `max_connections` when multiple replicas run.

---

## File Locations

| What | Where |
|------|-------|
| Port traits | `crates/domain/src/ports/video.rs`, `ports/task.rs`, `ports/transaction.rs` |
| Postgres implementations | `crates/infrastructure/src/postgres/video_repository.rs`, `task_repository.rs`, `transaction.rs` |
| Migrations | `migrations/*.sql` |
| Bootstrap (pool creation) | `crates/infrastructure/src/bootstrap.rs` |
| New repository | Add port trait in `domain/src/ports/`, implement in `infrastructure/src/postgres/`, add migration in `migrations/` |
