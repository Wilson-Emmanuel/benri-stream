# benri-stream

A minimal private video streaming service. Upload a video, get a shareable link, stream it.

---

## Clarifications & Assumptions

A few things weren't specified. I made assumptions and designed around them.

| # | Question | Assumption |
|---|----------|------------|
| 1 | Do videos auto-expire? | No expiry. TTL easy to add later |
| 2 | Which upload formats? | MP4, WebM, MOV, AVI, MKV — all transcoded to HLS |
| 3 | Any metadata on upload? | Title only (required) |
| 4 | Are links permanent? | Permanent, unguessable. No revocation or expiry |
| 5 | What if processing fails partway? | Keep what works — video ends earlier, no error |
| 6 | Resumable uploads? | No. Anonymous = no session to resume. User starts over |
| 7 | Expected upload volume? | Unknown. Architecture supports horizontal worker scaling |

---

## Requirements

**Functional**
- Anonymous video upload (up to 1GB, common formats)
- System generates a shareable link per video
- Browser-based streaming via shareable link
- Adaptive quality (low/medium/high) based on viewer's connection

**Non-Functional**
- **Time-to-stream > quality** — video becomes watchable before full processing completes
- **Horizontal scaling** — stateless workers, scale by adding instances
- **Cost efficiency** — CDN reduces origin egress, no local disk on workers

---

## Architecture

### System Overview

<!-- Replace with rendered diagram image -->

```
Svelte SPA → API Server (Axum) → PostgreSQL ← Worker (GStreamer)
                 ↕                                  ↕
                Redis (message queue)
                 ↕
         S3-Compatible Storage → CDN → Viewers
```

| Component | Role |
|-----------|------|
| **API Server** | Axum (Rust). Upload orchestration, status polling, video metadata. Issues presigned URLs for direct-to-storage uploads. Stateless |
| **Worker** | Separate process. Consumes tasks from Redis, transcodes via GStreamer reading from and writing directly to S3. No local disk — stateless compute |
| **PostgreSQL** | Video records, task records. Source of truth for all state |
| **Redis** | Message queue (List, LPUSH/RPOP) + distributed lock for outbox poller. Ephemeral — DB is the source of truth |
| **S3 Storage** | Uploaded files (temp) and HLS output (permanent). Presigned URLs for upload. CDN origin |
| **CDN** | Edge-caches HLS segments. Origin hit once per segment, viewers served from cache. Primary cost control |

### Upload & Processing Flow

<!-- Replace with rendered diagram image -->

1. Frontend validates file (type, size, header check)
2. `POST /api/videos/initiate` → API creates video record, returns presigned upload URL
3. Frontend uploads directly to S3 via presigned URL (file never touches the API server)
4. `POST /api/videos/{id}/complete` → API validates file signature + size via range read
5. API creates a task in the same DB transaction (outbox pattern)
6. Worker picks up the task, probes the file from S3, transcodes segment by segment
7. After first segment succeeds → shareable link is generated (guaranteed watchable)
8. Frontend polls `GET /api/videos/{id}/status` until the link appears

### Task System

<!-- Replace with rendered diagram image -->

Uses an outbox pattern to ensure no work is lost:

1. Use case writes task to DB in the same transaction as the business operation
2. Outbox poller (with distributed lock) picks up PENDING tasks in batch, publishes to Redis
3. Workers consume from Redis, dispatch to handlers
4. Stale recovery resets stuck tasks back to PENDING

DB is source of truth. Redis is temporary — messages removed once consumed. If Redis
loses data, the poller re-publishes from DB.

**Worker tuning**: Each worker has a configurable concurrency limit (how many tasks it
processes simultaneously). Each task type defines its own processing timeout — long
tasks like video transcoding get longer timeouts than short tasks. If the system grows
to include both long and short tasks, worker groups or per-type concurrency limits
prevent long-running tasks from starving shorter ones.

### Streaming

<!-- Replace with rendered diagram image -->

- Transcoded to HLS at three quality levels (360p, 720p, 1080p)
- All three levels produced simultaneously per segment — adaptive bitrate from the first streamable moment
- Player (hls.js) fetches master manifest from CDN, auto-selects quality
- Native HLS on iOS/Safari, hls.js for other browsers

### GStreamer Pipeline

<!-- Replace with rendered diagram image -->

GStreamer reads input from S3 via URL, decodes, encodes at three quality levels in
parallel, writes HLS segments directly back to S3. No local disk — workers are
stateless compute that scale horizontally.

---

## Architecture Decisions

### Upload-first, then transcode

Client uploads the full file to storage first, then the worker transcodes from the
stored file.

I considered stream-through transcoding (pipe upload bytes directly to the transcoder)
for even faster time-to-stream. But MP4 — the most common format — often has its `moov`
atom at the end of the file. The transcoder needs this metadata before it can start.
The client could relocate it in-browser (mp4box.js), but that means re-processing up to
1GB before upload even starts. On mobile, that's a bad experience.

Upload-first with progressive per-segment transcoding gets most of the benefit: the
video becomes watchable from the beginning shortly after upload completes.

### Presigned URL upload

The file uploads directly to S3 via presigned URL — never through the API server.

For files up to 1GB, routing through the server ties up connections for minutes per
upload. Presigned URL offloads the transfer to the storage provider. The server just
orchestrates: issue URL, validate on completion, queue processing.

The presigned URL includes a max size condition (1GB). The storage provider rejects
oversized files at the network level.

### GStreamer over FFmpeg

GStreamer (via `gstreamer-rs` Rust bindings) instead of shelling out to FFmpeg.

FFmpeg's HLS muxer writes to local filesystem. This forces temp files on the worker
node, making workers stateful — local disk becomes the scaling bottleneck. GStreamer's
pipeline model streams from S3 source to S3 sink with no local disk. Workers are pure
stateless compute.

GStreamer also integrates natively with Rust — the team officially maintains
`gstreamer-rs` with production-ready bindings. Rust-written plugins ship in official
GStreamer binaries. It's a first-class language in the ecosystem, not a wrapper.

### Redis message queue (outbox pattern)

Outbox pattern with Redis List as the message queue. DB is the source of truth for task
state.

I considered several alternatives:
- **Database polling** — workers poll for rows, lock them with `SELECT FOR UPDATE`, process directly. Puts constant pressure on the DB (polling, row locks, heartbeats). At scale the DB becomes the bottleneck.
- **Row locking for dedup** — `SELECT FOR UPDATE` to prevent duplicate pickup. Concentrates coordination on the DB. A distributed lock in Redis is lighter.
- **Postgres LISTEN/NOTIFY** — fire-and-forget. If the worker is down, the notification is lost.
- **Kafka, GCP Pub/Sub, SQS, RabbitMQ, etc.** — production-grade but heavy for two task types. Right choice at larger scale.

Redis works because it's already needed for the distributed lock in the outbox poller —
no extra infrastructure. If Redis loses data, the poller re-publishes PENDING tasks from
DB. RPOP/LPUSH gives FIFO ordering. Multiple workers consume from the same queue.

This keeps the DB light: the poller reads/updates in batch, workers touch the DB only
twice per task (read task data, write final status), coordination happens in Redis not DB.

Queue depth is the scaling signal — when it grows, add worker instances. Workers are
stateless, so scaling is trivial. The queue is behind a port trait; swapping to Kafka or
Pub/Sub later is an infrastructure change.

### Per-segment parallel transcoding

For each segment, all three quality levels are produced simultaneously — not sequentially
per tier.

Per-tier sequential means the entire video must finish at low quality before medium even
starts. Per-segment means the first seconds are available in all qualities immediately.
Viewers get adaptive bitrate from the first streamable moment.

Trade-off: uses more memory per job (three encoders concurrently), but better experience.

### GPU-accelerated transcoding

Workers use GPU-accelerated encoding where available. GStreamer auto-detects hardware
encoders at runtime — same pipeline code runs on GPU or falls back to CPU.

GPU encoding is significantly faster. The balance between worker count and instance
capability is a deployment decision: more powerful instances = fewer workers needed,
and vice versa. GPU is not required — CPU-only works at any scale, just needs more
instances.

### Shareable link after first segment

The link is generated only after the first segment is successfully produced.

I considered generating it earlier (on upload or after probe), but then the user would be
holding a link to something that might never work. Cleanup handles the record, but the
user may have already shared the link and viewers would see an error.

After first segment, the link is guaranteed watchable. This doesn't affect time-to-stream
— viewers can't watch until segments exist regardless of when the link was generated.

### CDN as core architecture

HLS segments served through CDN, not directly from storage.

Direct from storage means every viewer hits the origin — cost scales linearly with
viewers. CDN flips this: origin hit once per segment, edge nodes serve everyone else.
For video streaming, CDN is the cost-efficient baseline, not a nice-to-have.

### HLS over MPEG-DASH

Both are capable streaming formats. I went with HLS because it plays natively on
iOS/Safari with no extra libraries. DASH would need a JavaScript player on iOS. On other
browsers, hls.js handles HLS playback.

### Three quality levels (360p, 720p, 1080p)

Covers mobile on bad networks to desktop on fast connections. Two levels would miss
high-quality desktop. Four+ adds storage and processing time for marginal gain — the
gaps between three are already small enough for smooth adaptive switching.

### No resumable uploads

Uploads are anonymous — no user account to tie a partial upload back to. Resuming
requires some session state, which adds complexity to an otherwise stateless flow.
Frontend validation reduces wasted uploads. If this becomes a problem, a short-lived
upload session token could be added.

### Discard original file after processing

Only HLS output is kept. The original can be reconstructed from segments by remuxing
(no re-encoding, no quality loss). Keeping both roughly doubles storage per video.

### Partial failure keeps successful segments

By the time processing fails partway, the shareable link is already live — the uploader
has it, may have shared it, viewers may be watching. Marking the whole video as failed
would break an active viewing experience. The segments that succeeded are valid and
watchable. Total failure (probe fails, zero segments) never generates a link.

### Client-side + server-side validation

Frontend checks file type, size, and basic header before uploading — avoids wasting time
on a 1GB upload that'll be rejected. Server validates on complete via range read (file
signature, actual size from storage metadata) without downloading the whole file. Deep
validation happens during probe.

---

## Code Structure — DDD with Compile-Time Enforcement

The backend follows Domain-Driven Design with hexagonal architecture. Business logic
lives in the center (domain + application), isolated from infrastructure. Every external
system is behind a trait (port). Changing the database, storage, transcoder, or queue is
an infrastructure swap.

Rust has no built-in DI, so layer boundaries are enforced via **separate workspace crates**
— the compiler rejects imports that violate the dependency direction.

```
crates/
  domain/           ← entities, enums, port traits. Zero external deps
  application/      ← use cases (usecases/) + shared services (services/). Depends on domain only
  infrastructure/   ← port implementations (sqlx, S3, GStreamer, Redis)
  api/              ← Axum handlers. Composition root for HTTP
  worker/           ← task consumer + outbox poller. Composition root for background work
```

```
api, worker → application → domain ← infrastructure
              (application cannot import infrastructure — compiler enforced)
```

**Platform agnosticism** — every external system can be swapped at the infrastructure
layer:

| Concern | Current | Could swap to |
|---------|---------|--------------|
| Database | PostgreSQL (sqlx) | MySQL, CockroachDB |
| Storage | S3-compatible | GCS, Azure Blob |
| Transcoder | GStreamer | Cloud transcoding (AWS MediaConvert, GCP Transcoder) |
| Queue | Redis | Kafka, GCP Pub/Sub, AWS SQS |
| CDN | Cloudflare | CloudFront, Fastly |

---

## Development Approach

**Spec-Driven Development** — The `.spec/` directory is the design source of truth.
Business spec defines entities, use cases, user stories. Architecture spec defines
patterns, layer rules. Code implements the spec — no unspec'd features.

AI-assisted development reads the spec (via `.claude/`) to generate consistent code.
See [.claude/CLAUDE.md](.claude/CLAUDE.md) for the AI configuration.

**Testing** — written alongside implementation.

| Layer | Type | Approach |
|-------|------|----------|
| domain | Unit | Pure logic, no mocks |
| application | Unit | Mock port traits (`mockall`) |
| infrastructure | Integration | Real test DB (`testcontainers` or local) |
| api | Integration | Full HTTP stack with test DB |
| worker | Integration | Task handlers with test DB |

Workflow: **spec → implement → test**.

---

## Running

*Prerequisites: Rust, PostgreSQL, Redis, GStreamer dev libraries, Node.js*

```bash
# Backend
cp .env.example .env              # configure DB, S3, Redis
cargo build                       # build all crates
cargo run --bin api               # start API server
cargo run --bin worker            # start worker (separate terminal)

# Frontend
cd frontend
npm install
npm run dev                       # dev server with API proxy
```

<!-- TODO: Docker Compose setup -->

---

## Tech Stack

| Concern | Choice |
|---------|--------|
| Language | Rust |
| Web framework | Axum |
| Database | PostgreSQL + sqlx |
| Storage | S3-compatible (aws-sdk-s3) |
| Transcoding | GStreamer (gstreamer-rs) |
| Message queue | Redis (List as queue) |
| Frontend | Svelte + hls.js |
| CDN | Cloudflare (or similar) |
