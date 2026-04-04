# Data Store

Relational database (e.g., PostgreSQL) serving as the single source of truth for all
application state — video records and task records.

---

## What It Stores

| Table | Purpose | Managed by |
|-------|---------|------------|
| `videos` | Video entity — status, share token, title, format, upload key | Video use cases |
| `tasks` | Task entity — status, metadata, trace ID, attempt count, scheduling | Task system |

---

## Port and Implementation

**Port traits** (domain):
```rust
// crates/domain/src/ports/video.rs
pub trait VideoRepository: Send + Sync {
    async fn insert(&self, video: &Video) -> Result<(), RepositoryError>;
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError>;
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError>;
    async fn update_status(&self, id: &VideoId, status: VideoStatus) -> Result<(), RepositoryError>;
    // ...
}

// crates/domain/src/ports/task.rs
pub trait TaskRepository: Send + Sync {
    async fn create(&self, task: &Task) -> Result<Task, RepositoryError>;
    async fn find_pending(&self, limit: i32, before: DateTime<Utc>) -> Result<Vec<Task>, RepositoryError>;
    async fn mark_in_progress(&self, ids: &[TaskId], started_at: DateTime<Utc>) -> Result<(), RepositoryError>;
    // ...
}
```

Each repository trait lives in the domain alongside its entity. All methods are async
and return `Result`.

**Implementation** (infrastructure):

Uses sqlx with runtime queries (not compile-time `query!` macros, since those require
a live DB connection at build time).

```rust
// crates/infrastructure/src/postgres/video_repository.rs
pub struct PostgresVideoRepository { pool: PgPool }
impl VideoRepository for PostgresVideoRepository { ... }

// crates/infrastructure/src/postgres/task_repository.rs
pub struct PostgresTaskRepository { pool: PgPool }
impl TaskRepository for PostgresTaskRepository { ... }
```

---

## Migrations

SQL migration files managed by `sqlx-cli`. Applied at startup by the API server.

```bash
# Create a new migration
sqlx migrate add <name>

# Run migrations
sqlx migrate run
```

Migrations live in the project root:
```
migrations/
  001_create_videos.sql
  002_create_tasks.sql
  ...
```

---

## Connection Pooling

sqlx provides async connection pooling via `PgPool`. Configured at startup with max
connections. Both API server and worker create their own pool.

---

## Atomic Operations

Status transitions that must prevent race conditions (e.g., claiming a video for
processing, claiming a task from the outbox) use conditional updates:

```sql
UPDATE videos SET status = 'PROCESSING' WHERE id = $1 AND status = 'UPLOADED'
```

If the row was already claimed by another worker, this affects 0 rows — the caller
detects it and skips.

---

## Configuration

| Config | Env var | Default |
|--------|---------|---------|
| Database URL | `DATABASE_URL` | `postgres://localhost:5432/benri_stream` |
| Max connections (API) | hardcoded | 10 |
| Max connections (Worker) | hardcoded | 5 |

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| `VideoRepository` trait | `domain` | `src/ports/video.rs` |
| `TaskRepository` trait | `domain` | `src/ports/task.rs` |
| `RepositoryError` | `domain` | `src/ports/video.rs` (shared across repos) |
| `PostgresVideoRepository` | `infrastructure` | `src/postgres/video_repository.rs` |
| `PostgresTaskRepository` | `infrastructure` | `src/postgres/task_repository.rs` |
| Row mapping helpers | `infrastructure` | `src/postgres/*.rs` (in each repo file) |
| Migration files | project root | `migrations/*.sql` |
| Pool config | `infrastructure` | `src/config.rs` |
| Pool creation + migration runner | `api`, `worker` | `src/main.rs` |
