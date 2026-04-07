# System Architecture — benri-stream

```
┌──────────────┐
│  Frontend SPA  │
│              │
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
└─────────────────────────────┬───────────────────────────┘
                              │
                              ▼
                       ┌──────────────┐
                       │     CDN       │──▶ Viewers
                       └──────────────┘
```

**API Server** — Handles upload initiate/complete, status polling, video metadata.
Stateless. Reads/writes records in the data store. Issues presigned URLs for storage.

**Worker** — Separate process. Consumes tasks from the message queue, transcodes via
a pipeline that reads from object storage via presigned URL. Segments are buffered
locally, uploaded to storage as they complete, then deleted. Workers are stateless —
nothing persists between jobs. No direct communication with the API server.

**Data Store** (e.g., PostgreSQL) — Video records, task records. Source of truth for
all state.

**Message Queue** (e.g., Redis List) — Outbox poller pushes tasks, workers consume.
Notification channel only — data store is the source of truth.

**Object Storage** (e.g., S3-compatible) — Uploaded files (temp) and HLS segments +
manifests (permanent). Presigned URLs for direct upload. CDN reads from here as origin.

**CDN** (e.g., Cloudflare) — Sits in front of object storage for HLS segment delivery.
Caches segments at edge locations close to viewers. Origin hit once per segment — all
subsequent viewers served from edge cache. Primary cost control: reduces origin egress
to near-zero for popular videos.

**Frontend SPA** — Upload page and player page. Talks to API server for orchestration,
uploads directly to storage via presigned URL, streams HLS from CDN.

---

## Design Approach — Domain-Driven Design (DDD)

The backend follows DDD with hexagonal architecture (ports and adapters). The core idea:
business logic lives in the center (domain + application layers), completely isolated
from infrastructure concerns. The domain defines *what* the system needs (repository
traits, port traits), infrastructure provides *how* (Postgres, S3, Redis, GStreamer).

**Why DDD for this project**:
- **Testability** — domain and application logic can be tested with in-memory fakes.
  No database, S3, or GStreamer needed for business logic tests.
- **Swappability** — every external system is behind a trait. Changing the database,
  storage provider, transcoder, or message queue is an infrastructure change that doesn't
  touch business logic.
- **Clarity** — clear boundaries make it obvious where each piece of logic belongs.
  Business rules in domain, orchestration in application, technical plumbing in
  infrastructure, HTTP mapping in presentation.

The layer structure (domain → application → infrastructure → presentation) is enforced
at compile time via separate workspace crates. See [Workspace Crates](backend/workspace-crates.md).

---

## Development Approach — Spec-Driven + TDD

**Spec-Driven Development (SDD)**: The `.spec/` directory is the source of truth for
what to build and how. The business spec defines entities, use cases, and user stories.
The architecture spec defines patterns, decisions, and layer rules. Code implements
the spec — no features are added without being spec'd first. AI-assisted development
reads the spec to generate correct, consistent code.

**Test-Driven Development (TDD)**: Tests are written alongside implementation. Use
cases and services get unit tests with mocked port traits. Infrastructure and
presentation get integration tests with real dependencies (test DB, test storage).

The workflow: **spec → implement → test**.

---

## Technology Stack

| Concern | Choice                            | Notes                                                                   |
|---------|-----------------------------------|-------------------------------------------------------------------------|
| Language | Rust                              |                                                                         |
| Web framework | Axum                              | Tokio-native, Tower middleware                                          |
| Database | PostgreSQL + sqlx                 | Async, compile-time query verification                                  |
| Migrations | sqlx-cli                          |                                                                         |
| Object storage | S3-compatible                     | Via `aws-sdk-s3` crate                                                  |
| Transcoding | GStreamer                         | Via `gstreamer-rs` bindings. Reads from S3, uploads segments to S3 |
| Frontend | Svelte                            | SPA, two pages                                                          |
| HLS player | hls.js                            | Browser-side adaptive streaming                                         |
| CDN | Cloudflare (or similar)           | Free egress, edge caching                                               |
| Message queue | Queue (e.g. Redis List)           | DB is source of truth, queue is notification channel                    |
| Logging | `tracing` + `tracing-subscriber`  | Structured, JSON in prod                                                |

---

## Key Patterns

### Use Case Pattern

Each use case takes port traits as constructor params, defines nested `Input`, `Output`,
`Error` types, and owns its transaction boundary.

### Service Pattern

Shared logic extracted when two or more use cases need the same code. Services don't
own transactions — they participate in the caller's. Same constructor injection of
port traits.

### Port / Adapter Pattern

Domain defines traits (`VideoRepository`, `StoragePort`). Infrastructure implements
them (`PostgresVideoRepository`, `S3StorageClient`). Composition roots (`api`, `worker`)
wire implementations to traits at startup.

---

## Transaction Management

Most operations are single-statement writes or reads. These go through
pool-backed repository methods (e.g. `VideoRepository::update_status_if`,
`TaskRepository::create`) and inherit atomicity from Postgres itself —
one SQL statement, one implicit commit, no ceremony.

Multi-statement atomicity is handled by a domain port, `TransactionPort`,
with a closure-based API:

```rust
tx_port.run(Box::new(|scope| Box::pin(async move {
    scope.videos().update_status_if(&id, from, to).await?;
    TaskScheduler::schedule_in_tx(scope.tasks(), &metadata, None).await?;
    Ok(())
}))).await?;
```

The infrastructure impl opens a sqlx transaction, runs the closure, and
commits on `Ok` / rolls back on `Err` or panic. Use cases never see
sqlx types.

The `TxScope` passed into the closure exposes only the mutation traits
that actually need transactional bundling (`VideoMutations`,
`TaskMutations`). Single-op writes stay on the pool-backed repositories
where the ceremony would be pure overhead.

---

## Platform Agnosticism

Every external system is accessed through a port trait defined in the domain layer.
Infrastructure provides the implementation. This means any tool can be replaced by
swapping the infrastructure implementation — domain and application code don't change.

| Concern | Current implementation | Could be replaced with |
|---------|----------------------|----------------------|
| Database | PostgreSQL (sqlx) | MySQL, CockroachDB |
| Object storage | S3-compatible (aws-sdk-s3) | GCS, Azure Blob, local filesystem |
| Message queue | Queue (e.g. Redis List) | Kafka, GCP Pub/Sub, AWS SQS, RabbitMQ |
| Transcoder | GStreamer (gstreamer-rs) | FFmpeg, cloud transcoding service (AWS MediaConvert, GCP Transcoder API) |
| CDN | Cloudflare | CloudFront, Fastly, Bunny CDN |

This is not hypothetical flexibility — it's enforced by the compiler. The `application`
crate cannot import `infrastructure`, so use cases physically cannot depend on a
specific database driver, storage SDK, or queue client.

---

## Detailed References

| Topic | Location |
|-------|----------|
| Workspace crates, dependency rules, source layout, config | [backend/workspace-crates.md](backend/workspace-crates.md) |
| Data store (schema, repositories, migrations) | [backend/data-store.md](backend/data-store.md) |
| Object storage (S3 layout, presigned URLs) | [backend/storage-layout.md](backend/storage-layout.md) |
| Transcoding (pipeline, quality levels, GPU) | [backend/transcoding.md](backend/transcoding.md) |
| Task system and worker | [backend/task-system.md](backend/task-system.md) |
| Error handling across layers | [backend/error-handling.md](backend/error-handling.md) |
| Testing strategy and test layout | [backend/testing.md](backend/testing.md) |
| Observability (logging, tracing, metrics) | [backend/observability.md](backend/observability.md) |
| Frontend SPA architecture | [frontend/spa.md](frontend/spa.md) |
