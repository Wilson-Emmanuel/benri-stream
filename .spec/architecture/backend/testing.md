# Testing

Tests are written alongside implementation. Each layer has a different approach matching its responsibilities.

---

## Strategy by Layer

| Layer | Test type | How |
|-------|-----------|-----|
| `domain` | Unit | Pure logic — no mocks needed |
| `application` | Unit | Mock port traits (`mockall`) |
| `infrastructure` | Integration | Real test DB, real S3 |
| `api` | Integration | Full HTTP request through real DB |
| `worker` | Integration | Real DB, mock queue |

---

## Test Layout

Tests live in `crates/<crate>/tests/` as external test files, not inline `#[cfg(test)]` blocks.

```
crates/domain/tests/
  video_test.rs
  task_test.rs
  task_scheduler_test.rs

crates/application/tests/
  initiate_upload_test.rs
  complete_upload_test.rs
  process_video_test.rs
  ...

crates/infrastructure/tests/
  video_repository_test.rs
  task_repository_test.rs
  transaction_test.rs
  s3_client_test.rs
  redis_queue_test.rs
  quality_test.rs

crates/api/tests/
  initiate_upload_test.rs
  complete_upload_test.rs
  get_video_test.rs

crates/worker/tests/
  handler_dispatch_test.rs
```

---

## Test Configuration

Integration tests use local Postgres/Redis instances.

| Env var | Default |
|---------|---------|
| `TEST_DATABASE_URL` | `postgres://localhost:5432/benri_stream_test` |
| `TEST_REDIS_URL` | `redis://localhost:6379/1` |

Shared test utilities (DB setup, fixtures) live in `crates/infrastructure/src/testing.rs` so other crates can import them.
