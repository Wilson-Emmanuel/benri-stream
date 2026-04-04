---
name: implement
description: Implement a feature, use case, or change end-to-end following the spec and architecture guides.
argument-hint: <use case or description>
disable-model-invocation: true
---

Implement the following: $ARGUMENTS

Before writing any code, enter plan mode. Determine what's being implemented and read only the relevant spec files:

- **Use case** -> read `business-spec/video/video.md`
- **Crate structure / naming** -> read `architecture/backend/workspace-crates.md`
- **Data store** -> read `architecture/backend/data-store.md`
- **Storage** -> read `architecture/backend/storage-layout.md`
- **Transcoding** -> read `architecture/backend/transcoding.md`
- **Task system** -> read `architecture/backend/task-system.md`
- **Error handling** -> read `architecture/backend/error-handling.md`
- **Observability** -> read `architecture/backend/observability.md`
- **Testing** -> read `architecture/backend/testing.md`

Propose your plan. List every file to create or modify, which crate it belongs to, and any existing code to reuse.

After approval, implement in this order (skip layers that don't apply):

1. **Domain** (`crates/domain/src/`) — entities, enums, value objects, port traits
2. **Application** (`crates/application/src/`) — use case with nested Input, Output, Error. Services if shared logic needed
3. **Infrastructure** (`crates/infrastructure/src/`) — port implementations (sqlx repos, S3, GStreamer, Redis)
4. **Task handler** (if side effects) — metadata in domain, handler in `crates/worker/src/handlers/`, register in handler map
5. **Presentation** — Axum handlers in `crates/api/src/handlers/` or worker consumer wiring
6. **Tests** — unit tests for use cases (mock traits with `mockall`), integration tests for infrastructure

Do not add anything not described in the spec.
