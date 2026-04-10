# Data Store

PostgreSQL. Single source of truth for all application state.

---

## Tables

| Table | Purpose |
|-------|---------|
| `videos` | Video entity — status, share token, title, format, upload key |
| `tasks` | Task entity — status, metadata, trace_id, attempt count, scheduling |

The `tasks` table includes a `trace_id` column (VARCHAR(32)) that links each task back to the API request that created it.

---

## Ports and Implementation

**Port traits** (domain) — `VideoRepository`, `TaskRepository` in `domain/src/ports/`. All methods are async, return `Result`.

**Implementation** (infrastructure) — `PostgresVideoRepository`, `PostgresTaskRepository` in `infrastructure/src/postgres/`. Uses sqlx with runtime queries (not compile-time `query!` macros, since those require a live DB at build time).

---

## Migrations

SQL files managed by `sqlx-cli`, applied at startup by the API server.

```
migrations/
  001_init.sql
  ...
```

---

## Atomic Operations

Status transitions use conditional updates to prevent race conditions:

```sql
UPDATE videos SET status = 'PROCESSING' WHERE id = $1 AND status = 'UPLOADED'
```

If already claimed by another worker, this affects 0 rows and the caller skips.

---

## Configuration

| Config | Env var | Default |
|--------|---------|---------|
| Database URL | `DATABASE_URL` | `postgres://localhost:5432/benri_stream` |
| Max connections (API) | hardcoded | 10 |
| Max connections (Worker) | hardcoded | 5 |
