# System Architecture

```
┌──────────────┐
│  Frontend SPA │
└──────┬───────┘
       │
       ▼
┌──────────────┐    ┌─────────────────┐    ┌──────────────┐
│  API Server   │───▶│   Data Store     │◀───│   Worker      │
│              │    └─────────────────┘    │              │
│              │    ┌─────────────────┐    │              │
│              │───▶│  Message Queue   │◀───│              │
└──────┬───────┘    └─────────────────┘    └──────┬───────┘
       │                                           │
       ▼                                           ▼
┌─────────────────────────────────────────────────────────┐
│                     Object Storage                       │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
                       ┌──────────────┐
                       │     CDN       │──▶ Viewers
                       └──────────────┘
```

## Components

| Component | Role |
|-----------|------|
| **API Server** | Stateless. Upload orchestration, status polling, video metadata. Issues presigned URLs for storage. |
| **Worker** | Separate process. Consumes tasks from queue, transcodes via GStreamer, uploads HLS segments to storage. Stateless between jobs. |
| **Data Store** (PostgreSQL) | Source of truth for video and task records. |
| **Message Queue** (Redis List) | Notification channel. DB is authoritative; queue is ephemeral. |
| **Object Storage** (S3-compatible) | Uploaded files (temp) and HLS output (permanent). CDN reads from here as origin. |
| **CDN** (Cloudflare) | Edge-caches HLS segments. Origin hit once per segment; subsequent viewers served from cache. |
| **Frontend SPA** (Svelte) | Upload page and player page. Uploads directly to storage via presigned URL, streams HLS from CDN. |

---

## Technology Stack

| Concern | Choice | Notes |
|---------|--------|-------|
| Language | Rust | |
| Web framework | Axum | Tokio-native, Tower middleware |
| Database | PostgreSQL + sqlx | Async, runtime queries |
| Migrations | sqlx-cli | |
| Object storage | S3-compatible | `aws-sdk-s3` crate |
| Transcoding | GStreamer | `gstreamer-rs` bindings, hlssink2 |
| Message queue | Redis List | DB is source of truth, queue is notification channel |
| Frontend | Svelte | SPA, two pages |
| HLS player | hls.js | Browser-side adaptive streaming |
| CDN | Cloudflare | Free egress, edge caching |
| Logging | `tracing` + `tracing-subscriber` | Structured JSON |
| Metrics | `metrics` + Prometheus | Exposition on `/metrics` |

---

## Design Patterns

**Hexagonal architecture (ports and adapters)** — business logic in the center (domain + application), isolated from infrastructure. Domain defines traits (`VideoRepository`, `StoragePort`); infrastructure implements them (`PostgresVideoRepository`, `S3StorageClient`). Composition roots (`api`, `worker`) wire implementations at startup. Layer boundaries are compiler-enforced via separate workspace crates.

**Use case pattern** — each use case takes port traits as constructor params, defines nested `Input`, `Output`, `Error` types, owns its transaction boundary.

**Service pattern** — shared logic extracted when two or more use cases need the same code. Services participate in the caller's transaction.

---

## Transaction Management

Most operations are single-statement writes and inherit atomicity from Postgres itself.

Multi-statement atomicity uses `TransactionPort` with a closure-based API:

```
tx_port.run(|scope| {
    scope.videos().update_status_if(id, from, to)?;
    TaskScheduler::schedule_in_tx(scope.tasks(), metadata, None)?;
})
// commits on Ok, rolls back on Err
```

The `TxScope` exposes only mutation traits (`VideoMutations`, `TaskMutations`). Single-op writes stay on pool-backed repositories.

---

## Platform Agnosticism

Every external system is behind a port trait. Swapping the implementation is an infrastructure change that does not touch business logic.

| Concern | Current | Alternatives |
|---------|---------|-------------|
| Database | PostgreSQL (sqlx) | MySQL, CockroachDB |
| Object storage | S3-compatible (aws-sdk-s3) | GCS, Azure Blob |
| Message queue | Redis List | Kafka, SQS, RabbitMQ |
| Transcoder | GStreamer (gstreamer-rs) | FFmpeg, cloud transcoding |
| CDN | Cloudflare | CloudFront, Fastly |

---

## Detailed References

| Topic | Location |
|-------|----------|
| Workspace crates, dependency rules, config | [backend/workspace-crates.md](backend/workspace-crates.md) |
| Data store (schema, repositories, migrations) | [backend/data-store.md](backend/data-store.md) |
| Object storage (S3 layout, presigned URLs) | [backend/storage-layout.md](backend/storage-layout.md) |
| Transcoding pipeline | [backend/transcoding.md](backend/transcoding.md) |
| Task system and worker | [backend/task-system.md](backend/task-system.md) |
| Error handling | [backend/error-handling.md](backend/error-handling.md) |
| Testing strategy | [backend/testing.md](backend/testing.md) |
| Observability | [backend/observability.md](backend/observability.md) |
| Frontend SPA | [frontend/spa.md](frontend/spa.md) |
