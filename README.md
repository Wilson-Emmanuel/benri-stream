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
- **Cost efficiency** — CDN reduces origin egress, stateless workers

### How each requirement is fulfilled

| Requirement | How | Details |
|-------------|-----|---------|
| Upload up to 1GB | Presigned URL with size policy + server-side validation via range read | [UC-VID-001, UC-VID-002](.spec/business-spec/video/video.md) |
| Common formats | MP4, WebM, MOV, AVI, MKV — validated by file signature (magic bytes) | [Video spec](.spec/business-spec/video/video.md) |
| Anonymous upload | No auth, no accounts — by design | |
| Shareable link | Generated after first segment succeeds — link is guaranteed watchable | [UC-VID-005](.spec/business-spec/video/video.md), [Shareable link decision](#shareable-link-after-first-segment) |
| Browser streaming | HLS via hls.js (all browsers), native on Safari/iOS | [Frontend SPA](.spec/architecture/frontend/spa.md) |
| Time-to-stream > quality | Parallel encoding (low finishes first), 4-second segments, progressive status (PARTIAL) | [Parallel encoding decision](#parallel-encoding-over-sequential-passes), [Segment duration decision](#4-second-hls-segments) |
| Horizontal scaling (bonus) | Stateless workers (nothing persists between jobs), queue-based task dispatch, distributed lock for poller | [Task system](.spec/architecture/backend/task-system.md) |
| Cost efficiency (bonus) | CDN edge-caches segments (origin hit once), presigned URLs bypass API server, original file discarded after processing | [CDN decision](#cdn-as-core-architecture), [Presigned URL decision](#presigned-url-upload) |
| Consistent playback (bonus) | Adaptive bitrate HLS (3 quality levels), CDN edge serving | [Transcoding](.spec/architecture/backend/transcoding.md) |

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
| **Worker** | Separate process. Consumes tasks from queue, transcodes via GStreamer. Segments are buffered locally during transcoding, uploaded to S3 as they complete, then deleted. Nothing persists between jobs — workers are stateless |
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

- Transcoded to HLS at three quality levels (360p, 720p, 1080p), 4-second segments
- All three levels produced simultaneously per segment — adaptive bitrate from the first streamable moment
- Player (hls.js) fetches master manifest from CDN, auto-selects quality based on viewer's connection
- Works in all browsers: Safari plays HLS natively, all others use hls.js

### GStreamer Pipeline

One pipeline per transcode job. Video is decoded **once**, then a `tee` fans the
frames to three encoder branches (one per quality level) running on separate threads.
Audio, when present, is also decoded once, encoded once as AAC, and shared across all
levels via a second tee — audio quality doesn't change per tier so re-encoding would
be wasted work.

```
uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale(360p)  → x264enc → h264parse ─┐
               │                            ├─ queue → scale(720p)  → x264enc → h264parse ─┤
               │                            └─ queue → scale(1080p) → x264enc → h264parse ─┤
               │                                                                            ├──▶ mpegtsmux ──▶ hlssink3 (per level)
               └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ─────────┘
                  (only built if the source has an audio stream)
```

`uridecodebin3` (stable since GStreamer 1.22) is the modern streams-aware source with
more accurate HTTP buffering — less over-download from S3 presigned URLs compared to
the older `uridecodebin`. `hlssink3` writes 4-second HLS segments to a local temp
directory; each completed segment is uploaded to S3 and the local file deleted.
Workers are stateless — nothing persists between jobs.

See [transcoding spec](.spec/architecture/backend/transcoding.md) for details on each
element and the parallelism properties.

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

FFmpeg is typically invoked by shelling out — spawning a child process, parsing stdout,
managing lifecycle. GStreamer integrates natively via `gstreamer-rs` with in-process
pipeline control, signal callbacks when segments complete, and programmatic error
handling. Both buffer segments locally during transcoding — the difference is integration
quality, not disk usage.

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

### Parallel encoding over sequential passes

The input file is decoded once and fanned out to three encoders (360p, 720p, 1080p)
running simultaneously. Each encoder writes to its own output path — no conflicts.

I considered three alternatives:

| Approach | How it works | Time-to-first-segment | Total processing time | CPU at any moment |
|----------|-------------|----------------------|----------------------|-------------------|
| **Parallel** (chosen) | Decode once → 3 encoders simultaneously | ~4s (low finishes first naturally) | Shortest — one pass | High — 3 encoders |
| **Sequential per level** | Low pass → medium pass → high pass | ~4s (all CPU on low) | ~3x longer — 3 full passes, 3 decodes | Low — 1 encoder |
| **Low first, then medium+high parallel** | Low pass → medium+high simultaneously | ~4s (all CPU on low) | ~1.5x longer — 2 passes, 2 decodes | Varies |

All three approaches produce the first low-quality segment in roughly the same time
(low quality encoding is fast regardless). But parallel wins on total processing time
because it decodes the input **once** instead of two or three times. Sequential passes
re-read from storage and re-decode the entire file per pass — wasted I/O and CPU.

The tradeoff is higher peak CPU/memory (three encoders concurrently), but workers scale
horizontally — add more instances rather than more CPU per instance. Total work done is
actually less with parallel (one decode vs three).

### 4-second HLS segments

Segment duration directly affects time-to-stream: the viewer waits for at least one
full segment to be encoded before anything is watchable. Shorter segments = faster
first playback.

| Duration | Time-to-stream | File overhead | CDN efficiency |
|----------|---------------|---------------|----------------|
| 2s | ~2s | High — many small files, more HTTP requests per minute of video | More cache misses |
| **4s** (chosen) | **~4s** | **Moderate** | **Good** |
| 6s | ~6s | Low | Best — fewer, larger files |
| 10s | ~10s | Lowest | Best |

4 seconds balances fast time-to-stream with manageable file count. 2-second segments
roughly double the number of files and manifest entries, increasing storage listing
costs and player overhead for marginal time-to-stream gain. 6+ seconds delays first
playback noticeably.

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

### H.264 video codec

H.264 (via `x264enc`) for all encoded output, even though VP9 and AV1 are royalty-free
and would be cheaper to ship at scale.

| Codec | Browser support | HLS compatibility | License |
|-------|----------------|-------------------|---------|
| **H.264** (chosen) | Universal — every browser, every device | Native HLS spec | Patents (free for streaming) |
| H.265/HEVC | Safari yes, Chrome partial, Firefox no | HLS supports it but real-world compat is bad | Heavy patents |
| VP9 | Chrome/Firefox/Edge yes, Safari no for HLS | Not in standard HLS | Royalty-free |
| AV1 | Newer browsers only, slow encode | Not in standard HLS | Royalty-free |

For our requirement (browser playback via shareable link), H.264 is the only codec that
works everywhere with HLS. VP9/AV1 would require switching to MPEG-DASH, which loses
Safari's native HLS support and forces a JavaScript player on iOS. The royalty cost
for H.264 streaming is zero — patents only apply to encoder/decoder distribution, not
streaming delivery.

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

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with Docker Compose

That's it. Everything (API, worker, frontend, database, queue, storage, CDN) runs in
containers — no Rust, Node.js, or GStreamer needed on the host.

### Start everything

```bash
docker compose up --build
```

First build takes a few minutes (compiling Rust + installing GStreamer plugins).
Subsequent runs are fast.

| Service | Port | Purpose |
|---------|------|---------|
| Frontend | http://localhost:5173 | Svelte SPA — upload page and player |
| API | http://localhost:8080 | Backend HTTP API (proxied via frontend `/api`) |
| Worker | — | Background transcoder, no exposed port |
| Postgres | 5432 | Data store |
| Redis | 6379 | Message queue + distributed lock |
| MinIO | 9000 (API), 9001 (console) | S3-compatible object storage |
| CDN | 8888 | Nginx caching segments from MinIO |

Open http://localhost:5173 to use the app.

### Stopping

```bash
docker compose down       # stop everything (data preserved in volumes)
docker compose down -v    # stop and delete all data
```

---

## Tech Stack

| Concern | Choice | Local container |
|---------|--------|-----------------|
| Language | Rust | |
| Web framework | Axum | |
| Database | PostgreSQL + sqlx | `postgres:17` |
| Storage | S3-compatible (aws-sdk-s3) | `minio/minio` |
| Transcoding | GStreamer (gstreamer-rs) | `debian:bookworm-slim` + GStreamer plugins |
| Message queue | Redis (List as queue) | `redis:8` |
| Frontend | Svelte + hls.js | `nginx:alpine` (serves built static files) |
| CDN | Cloudflare (or similar) | `nginx:alpine` (cache simulator) |
