# Video

## Overview

A `Video` tracks the lifecycle of an uploaded file from upload through transcoding to streaming. Every video is anonymous (no accounts). The share token is the sole access path. There is no listing, search, or discovery.

Lifecycle: Initiate -> Upload to storage -> Complete -> Process -> Streamable.

---

## Definitions

### Attributes

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|
| `id` | Unique identifier | No | Internal system identifier |
| `share_token` | Text (21 chars) | Yes | Unguessable token for the shareable link. Null until the low tier's first HLS segment and master playlist are in storage (written during `Processing`). URL-safe |
| `title` | Text (1-100 chars) | No | User-provided title. Frontend pre-fills from filename |
| `format` | Video Format (see Enums) | No | Determined from MIME type on upload, validated via file signature on complete |
| `status` | Video Status (see Enums) | No | Current lifecycle state |
| `upload_key` | Text | No | Storage key for the uploaded file. Cleared after processing |
| `created_at` | Date/time | No | When the upload was initiated |

### Enums

#### Video Status

| Value | Description |
|-------|-------------|
| `PENDING_UPLOAD` | Presigned URL issued, waiting for client to upload |
| `UPLOADED` | File in storage, queued for processing |
| `PROCESSING` | Being transcoded. May already have a share token if the low tier's first segment has landed |
| `PROCESSED` | All quality tiers finished. Nothing more will be written to storage |
| `FAILED` | Processing failed. Scheduled for deletion |

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

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `title` | Yes | 1-100 chars. Frontend pre-fills from filename |
| `mime_type` | Yes | Must map to a supported Video Format |

**Guards**
1. Title is not blank and does not exceed 100 chars
2. MIME type maps to a supported Video Format

**Mutations**
- Create `Video` with `status = PENDING_UPLOAD`, `format` from MIME type, `share_token = null`
- Generate presigned upload URL with 1 GB max size condition (storage-enforced)

**Output**

| Field | Description |
|-------|-------------|
| `id` | Video ID (used for polling and completing) |
| `upload_url` | Presigned URL for direct upload to storage (PUT) |

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

Called after the client uploads directly to storage. Validates without downloading — reads magic bytes via range read and checks actual size from storage metadata.

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
| `ALREADY_COMPLETED` | Video is past PENDING_UPLOAD |

**Side Effects**

- On `FILE_TOO_LARGE` or `INVALID_FILE_SIGNATURE`: schedule `DeleteVideo` ([UC-VID-007](#uc-vid-007)) standalone. If the schedule fails, the safety-net sweep ([UC-VID-006](#uc-vid-006)) collects the orphan.
- On success: schedule `ProcessVideo` in the same transaction as the status update.

**Idempotency**: Not idempotent — calling again after completion returns `ALREADY_COMPLETED`.

---

### Get Video Status {#UC-VID-003}

**Actor**: Anyone (the uploader polls this after completing upload)

**Triggered by**: REST: `GET /api/videos/{id}/status`

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
| `share_url` | Full shareable URL. Null until share token is written (during Processing) |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `VIDEO_NOT_FOUND` | No video with this ID |

**Side Effects**: N/A

**Idempotency**: Idempotent — read-only.

---

### Get Video by Share Token {#UC-VID-004}

**Actor**: Anyone with the link

**Triggered by**: REST: `GET /api/videos/share/{share_token}`

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `share_token` | Yes | Path parameter |

**Guards**: N/A

**Mutations**: N/A

**Output**

| Field | Description |
|-------|-------------|
| `title` | |
| `stream_url` | HLS manifest URL if playable. Null if still processing |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `VIDEO_NOT_FOUND` | No video exists with this share token |

**Side Effects**: N/A

**Idempotency**: Idempotent — read-only.

---

### Process Video (System) {#UC-VID-005}

**Actor**: System — not user-facing

**Triggered by**: Worker consumes `ProcessVideo` task scheduled by [UC-VID-002](#uc-vid-002).

Probes the file, transcodes into adaptive HLS, and uploads segments progressively. The share token is written as soon as the low tier's first segment and the master playlist are in storage — the link goes live before the full transcode finishes. Once all tiers complete, the status flips to `PROCESSED`.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `video_id` | Yes | The video to process |

**Guards**
1. Video exists and status is `UPLOADED`
2. Original file exists in storage (validated by probe)

**Mutations**
1. Atomically set `status = PROCESSING` (only if still `UPLOADED`). If not, skip.
2. Probe the file — confirm decodability, capture stream info.
3. Start transcode pipeline. Segments and per-tier playlists upload progressively.
4. **Early share-link publish**: when the low tier's first segment and master playlist are durable, atomically write `share_token` (only if status is still `Processing`). Viewers get a playable low-quality stream that fills in with higher tiers as they land.
5. Wait for all tiers to finish.
6. On success: atomically set `status = PROCESSED`.
7. Delete original upload from storage (best-effort; orphans collected by UC-VID-006).

On probe or transcode failure:
- Atomically set `status = FAILED` and schedule `DeleteVideo` ([UC-VID-007](#uc-vid-007)) in the same transaction.
- If failure is after step 4 (token already issued), viewers briefly see a stream that stops advancing, then `VIDEO_NOT_FOUND` once deletion runs. The race window is minutes; acceptable for anonymous casual sharing.

Early publish adds one atomic write during `Processing` and moves the share-link visibility earlier. The status enum is unchanged, no partial-preservation on failure, no segment counting or branching state machine.

**Output**: N/A (system process)

**Error Codes**: N/A (failures recorded on the video entity)

**Side Effects**

- On failure: schedules `DeleteVideo` ([UC-VID-007](#uc-vid-007)) in the same transaction as the `FAILED` status update.

**Idempotency**: Not safely retryable. The `Uploaded -> Processing` claim guard makes re-runs a no-op. Configured with `max_retries = 1`; any failure is terminal. Safety-net sweep ([UC-VID-006](#uc-vid-006)) collects stuck rows after 24 hours. See [task-catalog.md](../task-system/task-catalog.md#processvideo).

---

### Cleanup Stale Videos (System) {#UC-VID-006}

**Actor**: System — not user-facing

**Triggered by**: Worker runs daily on schedule

Safety-net sweep for videos that should be removed but lack a `DeleteVideo` task (worker crash, task system unavailable). Does not delete directly — all deletion flows through `DeleteVideo` ([UC-VID-007](#uc-vid-007)).

**Input**: N/A

**Guards**: N/A

**Mutations**
- `PENDING_UPLOAD` older than 24 hours -> schedule `DeleteVideo`
- `UPLOADED` or `PROCESSING` older than 24 hours with no progress -> bulk-mark `status = FAILED`, then bulk-schedule `DeleteVideo`
- `FAILED` videos older than 24 hours -> schedule `DeleteVideo`
- `PROCESSED` videos kept indefinitely

The 24-hour window on FAILED gives the primary `DeleteVideo` task (scheduled at failure time) room to complete, avoids duplicate task rows, and gives operators time to inspect before permanent removal.

Scheduling is dedup-by-default: re-running does not create duplicate tasks while a previous one is active.

**Output**: N/A

**Error Codes**: N/A

**Side Effects**: Schedules `DeleteVideo` tasks for qualifying videos.

**Idempotency**: Idempotent — re-running schedules nothing new for videos with an active `DeleteVideo` task.

---

### Delete Video (System) {#UC-VID-007}

**Actor**: System — not user-facing

**Triggered by**: `DeleteVideo` task. Scheduled by:
- [UC-VID-002](#uc-vid-002) on rejection (`FILE_TOO_LARGE`, `INVALID_FILE_SIGNATURE`)
- [UC-VID-005](#uc-vid-005) on probe/transcode failure
- [UC-VID-006](#uc-vid-006) safety-net sweep

Single delete path for videos. Removes storage objects and the database record.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `video_id` | Yes | The video to delete |

**Guards**
1. Video record exists. If not (already deleted), task completes as `Skip`.

**Mutations** (sequential, each must succeed before the next)
1. `storage.delete_prefix("videos/{id}/")` — removes HLS output tree. No-op if empty.
2. `storage.delete_object(upload_key)` — removes original file. No-op if already gone.
3. `video_repo.delete(id)` — removes database row.

If any step fails, the task returns a retryable failure. Both storage operations tolerate "already deleted," so retries are safe.

**Output**: N/A

**Error Codes**: N/A (failures handled by task retry / dead letter)

**Side Effects**: N/A

**Idempotency**: Idempotent — re-running on missing video, prefix, or object is a no-op at every step.

---

## Limits and Quotas

| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max upload file size | 1 GB | Presigned URL policy + client check + guard on complete |
| Max title length | 100 chars | Guard on initiate |
| Stale upload/processing timeout | 24 hours | Cleanup task |
| Failed video retention | 24 hours | Cleanup task |
| Share token length | 21 chars | URL-safe, generated during processing |
