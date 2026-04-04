# Testing

Tests are written alongside implementation. Each layer has a different approach matching
its responsibilities.

---

## Strategy by Layer

| Layer | Test type | What to test | How |
|-------|-----------|-------------|-----|
| `domain` | Unit | Entity methods, value object validation, domain services | Pure logic — no mocks needed |
| `application` | Unit | Use cases, application services | Mock port traits (`mockall`) |
| `infrastructure` | Integration | Repository queries, storage operations, transcoder | Real test DB (`testcontainers` or local Postgres) |
| `api` | Integration | Full HTTP request -> use case -> DB | Real test DB, `axum::test` helpers |
| `worker` | Integration | Task handlers end-to-end | Real test DB, mock queue |

---

## Crates

| Crate | Purpose |
|-------|---------|
| `tokio::test` | Async test runtime |
| `mockall` | Generate mock implementations of port traits for unit tests |
| `testcontainers` | Spin up Postgres and Redis containers for integration tests |

---

## Test Layout

Tests live inside each crate. Unit tests use `#[cfg(test)] mod tests` inline or a
separate `tests/` directory mirroring source structure.

```
crates/domain/
  src/video/video.rs
  src/video/video_test.rs                              <- inline #[cfg(test)] or separate

crates/application/
  src/usecases/video/initiate_upload.rs
  tests/usecases/video/initiate_upload_test.rs         <- unit test, mocks port traits

crates/infrastructure/
  src/postgres/video_repository.rs
  tests/postgres/video_repository_test.rs              <- integration test, real DB
  src/storage/s3_client.rs
  tests/storage/s3_client_test.rs                      <- integration test, real or mock S3

crates/api/
  tests/handlers/video_test.rs                         <- integration test, full HTTP stack

crates/worker/
  tests/handlers/process_video_handler_test.rs         <- integration test, real DB
```

---

## Test Configuration

Integration tests that need a database or Redis use either:
- **testcontainers** — spins up a fresh Postgres/Redis container per test suite.
  No setup needed, but slower.
- **Local instances** — developer-configured Postgres/Redis. Faster, requires setup.

Test config reads from environment variables with test-specific defaults.

| Config | Env var | Default |
|--------|---------|---------|
| Test database URL | `TEST_DATABASE_URL` | `postgres://localhost:5432/benri_stream_test` |
| Test Redis URL | `TEST_REDIS_URL` | `redis://localhost:6379/1` |

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| Domain unit tests | `domain` | Inline `#[cfg(test)]` or `src/*/[name]_test.rs` |
| Use case unit tests | `application` | `tests/usecases/*/[name]_test.rs` |
| Repository integration tests | `infrastructure` | `tests/postgres/*_test.rs` |
| Storage integration tests | `infrastructure` | `tests/storage/*_test.rs` |
| HTTP integration tests | `api` | `tests/handlers/*_test.rs` |
| Worker handler tests | `worker` | `tests/handlers/*_test.rs` |
| Test config/utilities | `infrastructure` | `src/testing/mod.rs` (in `src` so other crates can import) |
