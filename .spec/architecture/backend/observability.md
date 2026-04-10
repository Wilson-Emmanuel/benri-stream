# Observability

---

## Trace ID Propagation

```
HTTP request
  → trace_id_middleware: reads X-Request-Id / traceparent header, or generates UUID
  → stores in request extensions + sets tokio task_local via with_trace_id()
  → TraceLayer opens root span with trace_id field

  → handler → use case
    → TaskScheduler reads current_trace_id() from task_local
    → stamps trace_id on the Task row

  → worker picks up task
    → reads trace_id from Task row
    → wraps dispatch in with_trace_id() scope + tracing span
    → all logs carry the originating request's trace_id
```

One trace_id connects the request through task creation to worker processing. Implementation in `crates/api/src/middleware.rs` and `crates/domain/src/task/trace_context.rs`.

---

## Logging

JSON structured logging via `tracing` + `tracing-subscriber`. Configured in `main.rs` of both `api` and `worker`.

| Level | When |
|-------|------|
| `info` | Significant operations (entity created, task published, processing started/completed) |
| `warn` | Recoverable issues (retry, stale recovery, validation rejection) |
| `error` | Unrecoverable failures (external system unreachable, permanent failure) |

Log errors closest to where they originate. Infrastructure logs the technical detail; the caller adds business context.

---

## Metrics

`metrics` crate with Prometheus exposition on `/metrics`.

**Task system metrics**:

| Metric | Type | Tags |
|--------|------|------|
| `task.published` | Counter | `metadata_type` |
| `task.received` | Counter | `metadata_type` |
| `task.succeeded` | Counter | `metadata_type` |
| `task.failed` | Counter | `metadata_type` |
| `task.processing_duration` | Histogram | `metadata_type`, `result` |

The delta between `task.published` and `task.received` indicates queue depth and whether workers need to scale.

---

## File Locations

| What | Where |
|------|-------|
| Trace ID middleware | `crates/api/src/middleware.rs` |
| Task-local context | `crates/domain/src/task/trace_context.rs` |
| Tracing subscriber setup | `crates/api/src/main.rs`, `crates/worker/src/main.rs` |
| Metrics recording | `crates/worker/src/consumer.rs` (task outcome counters) |
