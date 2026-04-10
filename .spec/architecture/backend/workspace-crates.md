# Workspace Crates

Separate workspace crates enforce dependency direction at compile time. If `application` doesn't list `infrastructure` in its `Cargo.toml`, it cannot import infrastructure types.

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

## Dependency Rules

```
api            -> application, infrastructure, domain
worker         -> application, infrastructure, domain
application    -> domain
infrastructure -> domain
domain         -> (nothing — only std, thiserror, serde, chrono, uuid, nanoid, tokio)
```

`api` and `worker` are composition roots — they wire infrastructure implementations to domain port traits. Application never imports infrastructure.

---

## Source Layout

```
crates/domain/src/
  lib.rs
  video/
  task/
    metadata/
    trace_context.rs
    scheduler.rs
    result.rs
  ports/
    video.rs, task.rs, storage.rs, transcoder.rs,
    transaction.rs, distributed_lock.rs, error.rs

crates/application/src/
  lib.rs
  usecases/video/
  services/

crates/infrastructure/src/
  lib.rs
  config.rs
  bootstrap.rs
  postgres/
    video_repository.rs, task_repository.rs, transaction.rs
  storage/
    s3_client.rs
  transcoder/
    gstreamer.rs, hls_uploader.rs, quality.rs
  redis/
    task_publisher.rs, task_consumer.rs, distributed_lock.rs
  testing.rs

crates/api/src/
  main.rs, lib.rs
  handlers/video.rs
  middleware.rs

crates/worker/src/
  main.rs, lib.rs
  consumer.rs, poller.rs, recovery.rs, system_checker.rs
  handlers/
    mod.rs, process_video.rs, cleanup_stale.rs, delete_video.rs
```

---

## Configuration

All config is in `crates/infrastructure/src/config.rs`, read from environment variables.

| Env var | Default |
|---------|---------|
| `DATABASE_URL` | `postgres://localhost:5432/benri_stream` |
| `BASE_URL` | `http://localhost:3000` |
| `S3_UPLOAD_BUCKET` | `benri-uploads` |
| `S3_OUTPUT_BUCKET` | `benri-stream` |
| `S3_REGION` | `us-east-1` |
| `S3_ENDPOINT` | (none — uses AWS default) |
| `S3_PUBLIC_ENDPOINT` | (none — defaults to `S3_ENDPOINT`; set when browser cannot reach internal hostname) |
| `CDN_BASE_URL` | `http://localhost:8888` |
| `REDIS_URL` | `redis://localhost:6379` |
| `LISTEN_ADDR` | `0.0.0.0:8080` |
| `QUALITY_TIERS` | `low,medium,high` (comma-separated; unknown entries dropped) |
| `WORKER_CONCURRENCY` | `1` |
