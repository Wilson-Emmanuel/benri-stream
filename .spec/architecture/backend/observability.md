# Observability

How to understand what the system is doing in production: logging, tracing, and metrics.

---

## Goals

1. **Trace an operation's full journey** — from the initial request through task creation,
   worker pickup, and processing completion. One trace ID ties the lifecycle together,
   even across processes (e.g., API server and worker).

2. **Know when to scale** — metrics on queue depth and processing duration tell when
   workers need to scale up or down.

3. **Debug without guessing** — structured logs with trace IDs and entity IDs. When
   something goes wrong, filter by an entity ID or trace ID and see the full story.

---

## Tracing

### How trace ID flows through the system

```
HTTP Request
  → Tower TraceLayer middleware (api crate)
    - creates root span, generates or propagates trace ID (W3C traceparent header)

  → Handler → Use case (application crate)
    - span context propagates automatically across .await
    - if the use case creates a Task, the current trace ID is stored on it

  → Worker picks up task
    - reads trace ID from Task record, creates child span under it
    - all logs carry the same trace ID as the originating request

  → Processing
    - child spans for each significant step (e.g., external calls, heavy computation)
```

One trace ID connects the request → task → worker processing. Searchable in any trace
backend (Jaeger, Tempo, etc.).

### Implementation

Rust's `tracing` crate unifies logging and distributed tracing. Spans propagate across
async `.await` boundaries automatically with Tokio.

**Per-function spans** — use `#[tracing::instrument]`:
```rust
#[tracing::instrument(skip(self), fields(entity_id = %input.id))]
async fn execute(&self, input: Input) -> Result<Output, Error> {
    // all logs here carry entity_id and the span's trace/span ID
}
```

**OpenTelemetry export** — `tracing-opentelemetry` bridges `tracing` spans to OTel.
Configured once at startup. If no export endpoint is set, traces appear in logs only.

### Where things live

| What | Crate | File |
|------|-------|------|
| Per-request root span | `api` | `src/middleware/trace.rs` — Tower `TraceLayer` config |
| Tracing subscriber setup | `api` | `src/main.rs` — JSON subscriber init |
| Tracing subscriber setup | `worker` | `src/main.rs` — JSON subscriber init |
| Function-level spans | any crate | `#[tracing::instrument]` on functions |
| Trace ID extraction for tasks | `application` | Inside use case that creates the task |
| Trace resumption from task | `worker` | `src/handlers/` — creates child span with stored trace ID |
| OTel export config | `infrastructure` | `src/observability/otel.rs` — reads endpoint from env |

---

## Logging

### What to log and when

| Level | When | Include |
|-------|------|---------|
| `info` | Significant operations (e.g., entity created, task published, processing started/completed) | Operation name, key entity IDs |
| `warn` | Recoverable issues (e.g., retry, stale recovery, validation rejection) | Same + error message |
| `error` | Unrecoverable failures (e.g., external system unreachable, processing permanently failed) | Same + full error context |

### What NOT to log

- **High-frequency internals** — if a function runs in a tight loop, the trace span
  covers it. Don't also log each iteration.
- **Large data** — file bytes, full payloads, serialized bodies. Log size/count instead.
- **Redundant context** — trace ID and span ID are attached automatically by `tracing`.

### Error logging location

Log errors closest to where they originate. Infrastructure logs the technical detail
(e.g., HTTP status, connection error). The caller (use case or handler) adds business
context (e.g., entity ID, task ID).

### Implementation

`tracing` macros with structured fields:

```rust
tracing::info!(entity_id = %id, status = "processing", "operation started");
tracing::error!(entity_id = %id, error = %e, "operation failed");
```

**Output format**: Structured JSON in all environments (`tracing-subscriber` with JSON
layer). 
Configured in `main.rs` of both `api` and `worker` crates.

---

## Metrics

### What to measure

Metrics are grouped by concern. Each system should define metrics relevant to its
operations. Common patterns:

**Task system** — drive scaling decisions:

| Metric pattern | Type | Tags | Why |
|---------------|------|------|-----|
| `task.published` | Counter | `metadata_type` | Tasks entering the queue |
| `task.received` | Counter | `metadata_type` | Tasks picked up by workers |
| `task.succeeded` | Counter | `metadata_type` | Successful completions |
| `task.failed` | Counter | `metadata_type` | Permanent failures |
| `task.processing_duration` | Histogram | `metadata_type`, `result` | Detect slow/stuck jobs |

**Domain-specific** — add counters and histograms for significant operations (e.g.,
uploads completed, processing duration, error rates by category, queued tasks, etc).

**API** — request duration by method/path/status.

### Scaling signal

The delta between `task.published` and `task.received` tells how much work is pending
in the queue. If it grows consistently, add worker instances. Workers are stateless —
scaling is trivial.

### Implementation

The `metrics` crate with `metrics-exporter-prometheus`:

```rust
metrics::counter!("task.published", "metadata_type" => &metadata_type).increment(1);
metrics::histogram!("task.processing_duration", "metadata_type" => &metadata_type).record(duration_secs);
```

Exposed on `/metrics` endpoint in Prometheus format.

### Where things live

| What | Crate | File |
|------|-------|------|
| Task metrics (published, received, succeeded, failed, duration) | `infrastructure` | `src/metrics/task_metrics.rs` |
| Domain-specific metrics | `infrastructure` | `src/metrics/` — one file per concern |
| Prometheus exporter endpoint (`/metrics`) | `api` | `src/main.rs` — exporter setup + route |
| HTTP request metrics | `api` | `src/middleware/metrics.rs` — Tower middleware |

