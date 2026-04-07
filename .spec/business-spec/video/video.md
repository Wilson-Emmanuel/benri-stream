# Video

## Overview

A `Video` tracks the lifecycle of an uploaded video file — from upload through processing
to streaming. Every video is anonymous (no user accounts). The share token is the only
way to access a video. There are no listing, search, or discovery.

Lifecycle: Initiate → Upload to storage → Complete → Process → Streamable.
See use cases below for details on each step.

## Changelog

| Date | Change | Author |
|------|--------|--------|
| 2026-04-03 | Initial spec | Wilson |
| 2026-04-06 | Split deletion into UC-VID-007 (`DeleteVideo` task). UC-VID-002 and UC-VID-005 schedule it directly on rejection/failure paths. UC-VID-006 becomes a safety-net sweep that schedules instead of mutating. FAILED retention reduced from 30 days to 24 hours. | Wilson |
| 2026-04-07 | Drop `PARTIAL` and `INCOMPLETE` from the lifecycle. Share token is now generated only when transcoding completes successfully (`Processing → Processed`). Probe + the upload-time validations are sufficient evidence the file is good; the previous "first segment" early-stream mechanism added significant complexity (callback plumbing, segment counting, branching error handling) without delivering the promised early time-to-stream — the implementation always ran the pipeline to completion before doing anything. Transcode failures now mark the whole video failed and schedule deletion; we no longer try to preserve partial output. | Wilson |

---

## Definitions

### Attributes

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|
| `id` | Unique identifier | No | Internal system identifier |
| `shareToken` | Text (21 chars) | Yes | Unique, unguessable token for the shareable link. Null until processing completes successfully. URL-safe |
| `title` | Text (1–100 chars) | No | User-provided title, displayed on the player page. Frontend pre-fills from filename |
| `format` | Video Format (see Enums) | No | Determined from MIME type on upload, validated via file signature on complete |
| `status` | Video Status (see Enums) | No | Current lifecycle state |
| `uploadKey` | Text | No | Storage key for the uploaded file. Cleared after processing |
| `createdAt` | Date/time | No | When the upload was initiated |

### Enums

#### Video Status

| Value | Description |
|-------|-------------|
| `PENDING_UPLOAD` | Presigned URL issued, waiting for client to upload to storage |
| `UPLOADED` | File in storage, queued for processing |
| `PROCESSING` | Being converted into streaming format |
| `PROCESSED` | Fully processed and watchable. Share token has been generated |
| `FAILED` | Processing failed at any point. No shareable link generated; video is scheduled for deletion |

#### Video Format

| Value | MIME Type | Extensions |
|-------|-----------|------------|
| `MP4` | `video/mp4` | .mp4 |
| `WEBM` | `video/webm` | .webm |
| `MOV` | `video/quicktime` | .mov |
| `AVI` | `video/x-msvideo` | .avi |
| `MKV` | `video/x-matroska` | .mkv |

---

## Use Cases

### Initiate Upload {#UC-VID-001}

**Actor**: Anyone (anonymous)

**Triggered by**: REST: `POST /api/videos/initiate`

Creates a video record and returns a presigned URL for direct upload to object storage.
The client should do basic validation first (file type, size, header check).

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `title` | Yes | 1–100 chars. Frontend pre-fills from filename |
| `mimeType` | Yes | Must map to a supported Video Format |

**Guards**
1. Title is not blank and does not exceed 100 chars
2. MIME type maps to a supported Video Format

**Mutations**
- Create `Video` with `status = PENDING_UPLOAD`, `format` from MIME type, `shareToken = null`
- Generate presigned upload URL with 1 GB max size condition (storage-enforced)

**Output**

| Field | Description |
|-------|-------------|
| `id` | Video ID (used for polling and completing) |
| `uploadUrl` | Presigned URL for direct upload to storage (PUT) |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `UNSUPPORTED_FORMAT` | MIME type not supported |
| `TITLE_REQUIRED` | Title is blank |
| `TITLE_TOO_LONG` | Title exceeds 100 chars |

**Side Effects**: N/A

**Idempotency**: Not idempotent — each call creates a new video record.

---

### Complete Upload {#UC-VID-002}

**Actor**: Anyone (the uploader, after uploading to storage)

**Triggered by**: REST: `POST /api/videos/{id}/complete`

Called after the client has uploaded the file directly to storage. Validates the file
without downloading it — reads the first few KB via range read to check the file
signature (magic bytes), and checks actual size from storage metadata.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `id` | Yes | Path parameter. Video ID from initiate |

**Guards**
1. Video exists and status is `PENDING_UPLOAD`
2. File exists in storage at the expected key
3. Actual file size does not exceed 1 GB (from storage metadata)
4. File signature (first bytes) matches declared format

**Mutations**
- Atomically set `status = UPLOADED` (only if still `PENDING_UPLOAD`)

**Output**

| Field | Description |
|-------|-------------|
| `id` | Video ID |
| `status` | `UPLOADED` |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `VIDEO_NOT_FOUND` | No video with this ID |
| `FILE_NOT_FOUND_IN_STORAGE` | File not at expected key |
| `FILE_TOO_LARGE` | Actual size exceeds 1 GB |
| `INVALID_FILE_SIGNATURE` | First bytes don't match declared format |
| `ALPROCESSED_COMPLETED` | Video is past PENDING_UPLOAD |

**Side Effects**

- On `FILE_TOO_LARGE` or `INVALID_FILE_SIGNATURE`: schedule a `DeleteVideo` task
  ([UC-VID-007](#uc-vid-007)) to immediately remove the rejected upload and its
  video record. The task is scheduled in the same DB transaction as the rejection.
  No side effect on the success path — the worker picks up `UPLOADED` videos independently.

**Idempotency**: Not idempotent — calling again after completion returns `ALPROCESSED_COMPLETED`.

---

### Get Video Status {#UC-VID-003}

**Actor**: Anyone (the uploader polls this after completing upload)

**Triggered by**: REST: `GET /api/videos/{id}/status`

Polling endpoint. Returns current status and the shareable link once it's available
(after processing completes successfully).

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `id` | Yes | Path parameter. Video ID |

**Guards**: N/A

**Mutations**: N/A

**Output**

| Field | Description |
|-------|-------------|
| `status` | Current status |
| `shareUrl` | Full shareable URL. Null until processing completes successfully |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `VIDEO_NOT_FOUND` | No video with this ID |

**Side Effects**: N/A

**Idempotency**: Idempotent — read-only.

---

### Get Video by Share Token {#UC-VID-004}

**Actor**: Anyone with the link

**Triggered by**: REST: `GET /api/videos/share/{shareToken}`

Fetches video metadata and streaming info for the player page.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `shareToken` | Yes | Path parameter |

**Guards**: N/A

**Mutations**: N/A

**Output**

| Field | Description |
|-------|-------------|
| `title` | |
| `streamUrl` | HLS manifest URL if playable. Null if still processing |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `VIDEO_NOT_FOUND` | No video exists with this share token |

**Side Effects**: N/A

**Idempotency**: Idempotent — read-only.

---

### Process Video (System) {#UC-VID-005}

**Actor**: System — not user-facing

**Triggered by**: Worker consumes the `ProcessVideo` task scheduled by
[UC-VID-002](#uc-vid-002) on successful upload completion.

Probes the file, transcodes it into adaptive HLS, and uploads the segments.
On success, the share token is generated and the status flips to `PROCESSED`
in a single atomic update.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `videoId` | Yes | The video to process |

**Guards**
1. Video exists and status is `UPLOADED`
2. Original file exists in storage (validated implicitly by the probe)

**Mutations**
1. Atomically set `status = PROCESSING` (only if still `UPLOADED`). If not, skip — another worker claimed it.
2. Probe the file — confirm it's a valid, decodable video and capture stream info
3. Transcode the full video into adaptive HLS, uploading segments to storage
4. On success → atomically set `shareToken` and `status = PROCESSED` in one statement
5. Delete original upload from storage. Best-effort — a failure here leaves
   an orphan that the cleanup safety-net (UC-VID-006) collects.

On probe failure or transcode failure:
- Atomically set `status = FAILED` and schedule a `DeleteVideo` task
  ([UC-VID-007](#uc-vid-007)) in the same DB transaction. The task removes
  the original upload and the video record.
- No share token generated — uploader sees FAILED when polling.

**Why no early-stream / partial-success handling**: probe + the upload-time
validations (client-side type/size check, server-side magic-byte signature
check) provide strong evidence the file is good before we start transcoding.
A previous design attempted to expose the video as soon as the first segment
was produced (a `PARTIAL` state) and to preserve partial output on mid-pipeline
failures (an `INCOMPLETE` state); both added significant complexity without
real benefit and were dropped on 2026-04-07.

**Output**: N/A (system process)

**Error Codes**: N/A (failures recorded on the video entity)

**Side Effects**

- On probe or transcode failure: schedules `DeleteVideo`
  ([UC-VID-007](#uc-vid-007)) in the same transaction as the `FAILED` status update.

**Idempotency**: Idempotent — re-running on `PROCESSED` is a no-op (the
initial `update_status_if(Uploaded → Processing)` returns false).

---

### Cleanup Stale Videos (System) {#UC-VID-006}

**Actor**: System — not user-facing

**Triggered by**: Worker runs daily on schedule

Acts as a **safety-net sweep** for videos that should be removed but were not
scheduled for deletion through the primary path (e.g. worker crashed before
[UC-VID-002](#uc-vid-002) or [UC-VID-005](#uc-vid-005) could schedule a `DeleteVideo`
task, or the task system was unavailable). This use case **does not delete files
or rows directly** — all deletion goes through `DeleteVideo` ([UC-VID-007](#uc-vid-007))
so retries, ordering, and dedup are uniform across the system.

**Input**: N/A

**Guards**: N/A

**Mutations**
- `PENDING_UPLOAD` older than 24 hours → schedule `DeleteVideo`
- `UPLOADED` or `PROCESSING` older than 24 hours with no progress →
  atomically set `status = FAILED` and schedule `DeleteVideo` (same transaction)
- `FAILED` videos older than 24 hours → schedule `DeleteVideo`
- `PROCESSED` videos are kept indefinitely

Scheduling is dedup-by-default: re-running the sweep will not create duplicate
`DeleteVideo` tasks for the same video while a previous task is still active
(see [task-system.md](../../architecture/backend/task-system.md) on
ordering-key dedup).

**Output**: N/A

**Error Codes**: N/A

**Side Effects**: Schedules `DeleteVideo` tasks for qualifying videos.

**Idempotency**: Idempotent — re-running schedules nothing new for videos
that already have an active `DeleteVideo` task.

---

### Delete Video (System) {#UC-VID-007}

**Actor**: System — not user-facing

**Triggered by**: `DeleteVideo` task. Scheduled by:
- [UC-VID-002](#uc-vid-002) on rejection (`FILE_TOO_LARGE`, `INVALID_FILE_SIGNATURE`)
- [UC-VID-005](#uc-vid-005) on probe failure or zero-segments failure
- [UC-VID-006](#uc-vid-006) safety-net sweep

Removes a video's storage objects and database record. This is the **single
delete path** for videos in the system — the only place where storage and DB
removal are co-located. Designed to be retried by the task system on transient
infrastructure failures.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `videoId` | Yes | The video to delete |

**Guards**
1. Video record exists. If not (already deleted), the task completes as `Skip` —
   running on a missing video is a no-op.

**Mutations** (in order — each step must succeed before the next runs)
1. `storage.delete_prefix("videos/{id}/")` — removes the entire HLS output tree
   (master.m3u8, per-quality playlists, .ts segments). No-op if the prefix is
   empty (e.g. probe-failed videos that never produced segments).
2. `storage.delete_object(upload_key)` — removes the original uploaded file.
   No-op if the object is already gone (e.g. successful processing already
   deleted it inline).
3. `video_repo.delete(id)` — removes the database row.

If any step fails, the task returns a retryable failure. The next attempt
re-checks the guard and re-runs the remaining steps. Both storage operations
are tolerant of "already deleted," so retries are safe.

**Output**: N/A

**Error Codes**: N/A (system process — failures handled by task retry / dead letter)

**Side Effects**: N/A

**Idempotency**: Idempotent — re-running on a missing video, missing prefix,
or missing object is a no-op at every step.

---

## Limits and Quotas

| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max upload file size | 1 GB | Presigned URL policy (storage-enforced) + client-side check + guard on complete |
| Max title length | 100 chars | Guard on initiate |
| Adaptive streaming | Videos are transcoded into multiple quality levels; the player automatically selects the best quality for the viewer's connection | Processing pipeline |
| Stale upload/processing timeout | 24 hours | Cleanup task |
| Failed video retention | 24 hours | Cleanup task |
| Share token length | 21 chars | Generated when processing completes successfully |
