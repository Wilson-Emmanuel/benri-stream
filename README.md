# benri-stream

A minimal private video streaming service. Upload a video, get a shareable link, stream it.

---

## Clarifications & Assumptions

| # | Question | Assumption |
|---|----------|------------|
| 1 | Do videos auto-expire? | No expiry. TTL easy to add later |
| 2 | Which upload formats? | MP4, WebM, MOV, AVI, MKV — all transcoded to HLS |
| 3 | Any metadata on upload? | Title only (required) |
| 4 | Are links permanent? | Permanent, unguessable. No revocation or expiry |
| 5 | What if processing fails partway? | Whole video marked FAILED, scheduled for deletion |
| 6 | Resumable uploads? | No. Anonymous = no session to resume |
| 7 | Expected upload volume? | Unknown. Architecture supports horizontal scaling |

---

## Requirements

**Functional**
- Anonymous video upload (up to 1 GB, common formats)
- System generates a shareable link per video
- Browser-based streaming via shareable link
- Adaptive quality (low/medium/high) based on viewer's connection

**Non-Functional**
- **Time-to-stream > quality** — video becomes watchable before full processing completes
- **Horizontal scaling** — stateless workers, scale by adding instances
- **Cost efficiency** — CDN caches segments, presigned URLs bypass API, original discarded after processing

---

## Architecture

### System Overview

![System Architecture](design%20assets/High%20level%20%20System%20Architecture%20Diagram.png)

| Component                       | Role |
|---------------------------------|------|
| **API Server**                  | Axum (Rust). Upload orchestration, status polling, video metadata. Issues presigned URLs for direct-to-storage uploads. Stateless |
| **Worker**                      | Separate process. Consumes tasks from queue, transcodes via GStreamer. Segments uploaded to S3 as they complete. Stateless — nothing persists between jobs |
| **(Database) PostgreSQL**       | Video records, task records. Source of truth for all state |
| **(Message Queue) Redis List**  | Message queue (List, LPUSH/RPOP) + distributed lock for outbox poller. Ephemeral — DB is the source of truth |
| **Object Store(S3 compatible)** | Uploaded originals (temp) and HLS output (permanent). Presigned URLs for upload. CDN origin |
| **CDN**                         | Edge-caches HLS segments. Origin hit once per segment, viewers served from cache |

### Upload & Processing Flow

![Upload and Processing Sequence](design%20assets/Video%20Upload%20And%20Processing%20Sequence%20%20Diagram.png)

1. Frontend validates file (type, size, header)
2. `POST /api/videos/initiate` → API creates video record, returns presigned upload URL
3. Frontend uploads directly to S3 via presigned URL (file never touches the API server)
4. `POST /api/videos/{id}/complete` → API validates file signature + size via range read
5. API creates a ProcessVideo task in the same DB transaction (outbox pattern)
6. Worker picks up the task, probes the file from S3, transcodes segment by segment
7. First low-tier segment uploaded → share link published immediately (video playable while encoding continues)
8. Frontend polls `GET /api/videos/{id}/status` until the link appears, user can copy and share

> Full use case specs: [Video entity & use cases](.spec/business-spec/video/video.md) · [UI interactions](.spec/business-spec/video/video.ui.md) · [User stories](.spec/business-spec/user-stories/anonymous-user.md)

### Task System

Uses an outbox pattern so no work is lost:

1. Use case writes task to DB in the same transaction as the business operation
2. Outbox poller (with distributed lock) picks up PENDING tasks in batch, publishes to Redis List
3. Workers consume from Redis, dispatch to typed handlers
4. Stale recovery resets stuck tasks back to PENDING

DB is source of truth. Redis is ephemeral — if it loses data, the poller re-publishes from DB.

> Read More Here: [Task system architecture](.spec/architecture/backend/task-system.md) · [Task catalog](.spec/business-spec/task-system/task-catalog.md)

### GStreamer Pipeline

One pipeline per transcode job. Video is decoded **once**, then a `tee` fans the frames
to three encoder branches running on separate threads. Audio (when present) is decoded
once, encoded once as AAC, and shared across all levels.

```
uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale(360p)  → x264enc → h264parse ─┐
               │                            ├─ queue → scale(720p)  → x264enc → h264parse ─┤
               │                            └─ queue → scale(1080p) → x264enc → h264parse ─┤
               │                                                                            ├──▶ hlssink2 (per level)
               └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ─────────┘
```

Each completed segment is uploaded to S3, the local file deleted. Workers are stateless.

> See Detail Here: [Transcoding architecture](.spec/architecture/backend/transcoding.md)

---

## Architecture Decisions

### Upload-first, then transcode

Client uploads the full file to storage first, then the worker transcodes from the stored
file. I considered stream-through transcoding (pipe upload bytes directly to the
transcoder), but MP4 — the most common format — often has its `moov` atom at the end.
The transcoder needs this metadata before it can start. Upload-first with progressive
per-segment output gets most of the benefit: the video becomes watchable shortly after
upload completes.

### Presigned URL upload

Files upload directly to S3 via presigned URL — never through the API server. For files
up to 1 GB, routing through the server ties up connections for minutes per upload.
Presigned URLs offload the transfer to the storage provider. The server just orchestrates:
issue URL, validate on completion, queue processing.

### GStreamer over FFmpeg

GStreamer (via `gstreamer-rs` Rust bindings) instead of shelling out to FFmpeg. GStreamer
integrates natively with in-process pipeline control, signal callbacks, and programmatic
error handling. The team officially maintains `gstreamer-rs` with production-ready
bindings — Rust is a first-class language in the GStreamer ecosystem.

### Redis message queue (outbox pattern)

Outbox pattern with Redis List as the message queue. I considered:
- **Database polling** — constant pressure on the DB with row locks and heartbeats
- **Postgres LISTEN/NOTIFY** — fire-and-forget; missed if worker is down
- **Kafka/SQS/RabbitMQ** — production-grade but at this level.

Redis works because it's already needed for the distributed lock and possibly caching if scope gets expanded. If it loses data, the
poller re-publishes from DB. The queue is behind a port trait — swapping to Kafka or
Pub/Sub is an infrastructure change, not a redesign.

### Parallel encoding over sequential passes

The input is decoded once and fanned out to three encoders (360p, 720p, 1080p) running
simultaneously. Sequential passes would decode the full file two more times — wasted I/O
and CPU. The tradeoff is higher peak CPU per job, but workers scale horizontally so
adding instances is cheaper than re-decoding in terms of time-to-stream.

### 4-second HLS segments

Segment duration directly affects time-to-stream — the viewer waits for at least one
full segment before anything is playable. Shorter segments mean faster first playback but
more files and HTTP requests. 4 seconds balances fast startup with manageable file count
and good CDN cache behavior.

### Early share-link publishing

The share link is published the moment the first low-tier segment and master playlist
land in storage — while the video is still Processing. The viewer gets a playable stream
immediately; quality improves as higher tiers finish encoding. If the transcode fails
after the link is published, the video is marked FAILED and scheduled for deletion
(the viewer briefly sees a broken stream, then VIDEO_NOT_FOUND — minutes at most).

I considered waiting until all tiers finish (simpler, guaranteed-complete stream) but
this directly contradicts the "time-to-stream > quality" requirement. Early publish
delivers a watchable video within seconds of upload completing, without introducing
new states or complex failure recovery.

### CDN as core architecture

HLS segments served through CDN. Direct from storage means every viewer hits the origin
and cost scales linearly. CDN helps to prevent this: origin hit once per segment, edge nodes serve
everyone else. For video streaming, CDN is the cost-efficient baseline.

### HLS over MPEG-DASH

HLS plays natively on iOS/Safari with no extra libraries. DASH would need a JavaScript
player everywhere. On non-Apple browsers, hls.js handles playback.

### H.264 video codec

H.264 via `x264enc`. H.265/HEVC is HLS-compatible but lacks Firefox and Chrome support
without paid licenses. VP9 and AV1 are royalty-free but not part of the HLS spec —
using them would require MPEG-DASH, losing Safari's native playback. H.264 is the
widest common denominator for HLS across all browsers.

### Other decisions

- **Three quality levels (360p, 720p, 1080p)** — covers mobile to desktop. Configurable via `QUALITY_TIERS` env var (e.g. `low` for faster dev iteration). See [Worker tuning](#worker-tuning).
- **No resumable uploads** — anonymous = no session to resume. Frontend validation reduces wasted uploads.
- **Discard original after processing** — only HLS output kept. Original reconstructable by remuxing segments (no quality loss).
- **Wholesale failure on transcode error** — whole video marked FAILED. No partial preservation. Simpler model, same UX (share link either works or doesn't exist).
- **Client-side + server-side validation** — frontend checks type/size before upload. Server validates via range read (file signature, size) without downloading the whole file.
- **GPU-accelerated encoding** — GStreamer auto-detects hardware encoders at runtime. Same pipeline code runs on GPU or falls back to CPU.

---

## Code Structure

Domain-Driven Design with hexagonal architecture, enforced at compile time via separate
workspace crates. The compiler rejects imports that violate layer boundaries.

```
crates/
  domain/           ← entities, enums, port traits. Zero external deps
  application/      ← use cases + services. Depends on domain only
  infrastructure/   ← port implementations (sqlx, S3, GStreamer, Redis)
  api/              ← Axum handlers. Composition root for HTTP
  worker/           ← task consumer + outbox poller. Composition root for background work
```

```
api, worker → application → domain ← infrastructure
              (application cannot import infrastructure — compiler enforced)
```

Every external system is behind a port trait — swappable at the infrastructure layer:

| Concern | Current | Could swap to |
|---------|---------|--------------|
| Database | PostgreSQL (sqlx) | MySQL, CockroachDB |
| Storage | S3-compatible | GCS, Azure Blob |
| Transcoder | GStreamer | AWS MediaConvert, GCP Transcoder |
| Queue | Redis | Kafka, SQS, Pub/Sub |
| CDN | Nginx (caching proxy) | Cloudflare, CloudFront, Fastly |

> Full architecture specs: [System overview](.spec/architecture/system-architecture.md) · [Crate structure](.spec/architecture/backend/workspace-crates.md) · [Data store](.spec/architecture/backend/data-store.md) · [Storage layout](.spec/architecture/backend/storage-layout.md) · [Error handling](.spec/architecture/backend/error-handling.md) · [Observability](.spec/architecture/backend/observability.md)

---

## Development Approach

**Spec-Driven** — the `.spec/` directory is the design source of truth. Business spec
defines entities, use cases, user stories. Architecture spec defines patterns, layer
rules, conventions. Code implements the spec.

**Testing** — written alongside implementation.

| Layer | Type | Approach |
|-------|------|----------|
| domain | Unit | Pure logic, no mocks |
| application | Unit | Mock port traits (`mockall`) |
| infrastructure | Integration | Real test DB |
| api | Integration | Full HTTP stack with test DB |
| worker | Integration | Task handlers with test DB |

---

## Known Limitations

- **Early-publish playback (Safari/Chrome)** — During transcoding, the worker publishes
  a growing HLS EVENT playlist. Firefox (via hls.js) handles this perfectly: plays
  what's available, buffers at the edge, resumes when new segments land. Safari uses
  native HLS and stops at the current edge instead of waiting. Chrome is inconsistent.
  Both work correctly once all tiers finish and `#EXT-X-ENDLIST` is written. I have time limitation to investigate a workaround.

---

## Running

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with Docker Compose

Everything runs in containers — no Rust, Node.js, or GStreamer needed on the host.

### Start everything

```bash
docker compose up -d --build
```

First build takes a few minutes (compiling Rust + installing GStreamer plugins).
Subsequent runs are fast.

| Service | URL | Purpose |
|---------|-----|---------|
| Frontend | http://localhost:18080 | Upload page and video player |
| API | http://localhost:8080 | Backend HTTP API (also proxied via frontend `/api`) |
| MinIO Console | http://localhost:9001 | S3-compatible storage admin (minioadmin/minioadmin) |
| Worker | — | Background transcoder, no exposed port |

### Worker tuning

Set these on the `worker` service in `docker-compose.yml` under `environment:`:

| Variable | Default | Purpose |
|----------|---------|---------|
| `QUALITY_TIERS` | `low,medium,high` | HLS quality tiers (360p, 720p, 1080p). All encoded in parallel. Override to `low` for faster dev iteration. |
| `WORKER_CONCURRENCY` | `3` | Max concurrent tasks per worker. The ordering key prevents two attempts on the same video regardless of this setting. |

### Stopping

```bash
docker compose down       # stop (data preserved in volumes)
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
| Frontend | Svelte + hls.js | `nginx:alpine` (serves built SPA) |
| CDN | Nginx (caching proxy) | `nginx:alpine` |
