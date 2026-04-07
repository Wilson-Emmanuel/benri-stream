# Workspace Crates

Rust has no built-in DI or layer enforcement. Separate workspace crates — one per DDD
layer — ensure the **compiler enforces dependency direction**. If `application` doesn't
list `infrastructure` in its `Cargo.toml`, it cannot import infrastructure types.

---

## Crate Map

```
crates/
  domain/           <- entities, value objects, repository traits, ports
  application/      <- use cases + services (depends on domain only)
  infrastructure/   <- port implementations (sqlx, S3, GStreamer, Redis)
  api/              <- Axum handlers, router, middleware (composition root for HTTP)
  worker/           <- task consumer, outbox poller (composition root for background work)
```

## Dependency Rules (enforced by Cargo.toml)

```
api            -> application, infrastructure, domain
worker         -> application, infrastructure, domain
application    -> domain
infrastructure -> domain
domain         -> (nothing — only std, thiserror, serde, chrono, uuid, nanoid)
```

`api` and `worker` are composition roots — they wire infrastructure implementations to
domain port traits. Application never imports infrastructure.

---

## What Each Crate Contains

**domain** — Zero framework dependencies.
Contains entities, enums, value objects, repository traits, and port traits. Organized
by context (`video/`, `task/`). Naming: entities are nouns, ports are
`[Concept]Repository` or `[Concept]Port`, enums are `[Entity]Status`, `[Entity]Format`, etc.

**application** — Depends on domain only. Two top-level modules:
- `usecases/` — organized by context. Each use case represents a single business
  operation with nested `Input`, `Output`, `Error` types. Use cases own transaction
  boundaries. Named `[Verb][Subject]UseCase`.
- `services/` — organized by context. Shared logic used by multiple use cases.
  Services don't own transactions — they participate in the caller's.

Both take port traits as constructor parameters. Task metadata types also live here.

**infrastructure** — Depends on domain only.
Implements all port traits. Repository impls use sqlx, storage uses aws-sdk-s3,
transcoder uses gstreamer-rs, queue uses the configured message broker. Also owns
DB migrations. Naming: `[Concept]RepositoryImpl`, `[Concept]Client`.

**api** — Composition root for HTTP.
Route definitions, request/response DTOs, maps between DTOs and use case types. Wires
infrastructure impls to port traits at startup.

**worker** — Composition root for background processing.
Runs the outbox poller, consumes from the queue, dispatches to handlers, runs stale
recovery and system task checker. Each handler is `[Action]TaskHandler`.

---

## Source Layout

```
crates/domain/src/
  lib.rs
  video/                  <- Video entity, VideoStatus, VideoFormat
  task/                   <- Task entity, TaskStatus, TaskMetadata trait, TaskResult
  ports/
    video.rs              <- VideoRepository, StoragePort, TranscoderPort
    task.rs               <- TaskRepository, TaskPublisher, TaskConsumer

crates/application/src/
  lib.rs
  usecases/
    video/                <- InitiateUploadUseCase, CompleteUploadUseCase, etc.
    task/                 <- ProcessVideoUseCase, CleanupStaleVideosUseCase, etc.
  services/
    video/                <- shared logic across video use cases

crates/infrastructure/src/
  lib.rs
  config.rs               <- AppConfig (reads env vars: DB, S3, Redis, CDN)
  postgres/
    video_repository.rs   <- impl VideoRepository
    task_repository.rs    <- impl TaskRepository
    migrations/           <- SQL migration files
  storage/
    s3_client.rs          <- impl StoragePort
  transcoder/
    gstreamer.rs          <- impl TranscoderPort
  redis/
    task_publisher.rs     <- impl TaskPublisher
    task_consumer.rs      <- impl TaskConsumer
    distributed_lock.rs   <- acquire/release lock with TTL
  metrics/
    task_metrics.rs       <- task counters and histograms
  observability/
    otel.rs               <- OpenTelemetry export config

crates/api/src/
  main.rs                 <- router, tracing subscriber, wiring, server startup
  handlers/
    video.rs              <- Axum handlers for video endpoints
  middleware/
    trace.rs              <- Tower TraceLayer config
    metrics.rs            <- HTTP request metrics

crates/worker/src/
  main.rs                 <- consumer loop, wiring
  consumer.rs             <- task consumer (pop from queue, dispatch, write result)
  poller.rs               <- outbox poller with distributed lock
  recovery.rs             <- stale task recovery
  system_checker.rs       <- system task checker
  handlers/
    mod.rs                <- handler dispatch map
    process_video.rs      <- ProcessVideoTaskHandler
    cleanup_stale.rs      <- CleanupStaleVideosTaskHandler
```

---

## Configuration

All config is in `crates/infrastructure/src/config.rs`, read from environment variables.

| Config | Env var | Default |
|--------|---------|---------|
| Database URL | `DATABASE_URL` | `postgres://localhost:5432/benri_stream` |
| Base URL | `BASE_URL` | `http://localhost:3000` |
| S3 upload bucket (private) | `S3_UPLOAD_BUCKET` | `benri-uploads` |
| S3 output bucket (public-read) | `S3_OUTPUT_BUCKET` | `benri-stream` |
| S3 region | `S3_REGION` | `us-east-1` |
| S3 endpoint | `S3_ENDPOINT` | (none — uses AWS default) |
| CDN base URL | `CDN_BASE_URL` | `http://localhost:8888` |
| Redis URL | `REDIS_URL` | `redis://localhost:6379` |
| Listen address | `LISTEN_ADDR` | `0.0.0.0:8080` |
| OTel endpoint | `OTEL_ENDPOINT` | (none — traces in logs only) |
