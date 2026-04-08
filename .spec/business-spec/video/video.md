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
| 2026-04-09 | Early share-link publishing: share token is now written during `Processing` the moment the low tier's first HLS segment + `master.m3u8` land in storage, rather than at the end of the full transcode. Video status stays at `Processing` until all tiers finish — only the token moves earlier. This reintroduces the "early time-to-stream" goal that the 2026-04-07 change removed, but without the `PARTIAL` / `INCOMPLETE` state machine that made the previous attempt complex. The trigger is a single one-shot event fired by the transcoder when the first segment and master playlist are both durable; no segment counting, no branching state. If the transcode fails later (after the token was issued) we still go straight to `FAILED` + `DeleteVideo`; viewers holding the link see `VIDEO_NOT_FOUND` once delete runs. Accepted race, see UC-VID-005. | Wilson |

---

## Definitions

### Attributes

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|
| `id` | Unique identifier | No | Internal system identifier |
| `share_token` | Text (21 chars) | Yes | Unique, unguessable token for the shareable link. Null until the low tier's first HLS segment and the master playlist are both in storage (written during `Processing`, before the full transcode finishes). URL-safe |
| `title` | Text (1–100 chars) | No | User-provided title, displayed on the player page. Frontend pre-fills from filename |
| `format` | Video Format (see Enums) | No | Determined from MIME type on upload, validated via file signature on complete |
| `status` | Video Status (see Enums) | No | Current lifecycle state |
| `upload_key` | Text | No | Storage key for the uploaded file. Cleared after processing |
| `created_at` | Date/time | No | When the upload was initiated |

### Enums

#### Video Status

| Value | Description |
|-------|-------------|
| `PENDING_UPLOAD` | Presigned URL issued, waiting for client to upload to storage |
| `UPLOADED` | File in storage, queued for processing |
| `PROCESSING` | Being converted into streaming format |
| `PROCESSED` | Fully processed. All quality tiers finished encoding and every segment is in storage. Note: a video can be watchable *before* reaching this state — the share token and master playlist are published earlier, as soon as the low tier's first segment lands (see UC-VID-005). `PROCESSED` specifically means "nothing more will be written" |
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
| `ALREADY_COMPLETED` | Video is past PENDING_UPLOAD |

**Side Effects**

- On `FILE_TOO_LARGE` or `INVALID_FILE_SIGNATURE`: schedule a `DeleteVideo` task
  ([UC-VID-007](#uc-vid-007)) to remove the rejected upload and its video record.
  The schedule is standalone (no enclosing transaction) — there is no business
  mutation in the rejection path to bundle with, since the video stays in
  `PENDING_UPLOAD`. If the schedule itself fails, the safety-net sweep
  ([UC-VID-006](#uc-vid-006)) collects the orphaned video on its next run.
- On the success path: schedule a `ProcessVideo` task in the same DB transaction
  as the `PENDING_UPLOAD → UPLOADED` status update — the task must exist if and
  only if the status update commits.

**Idempotency**: Not idempotent — calling again after completion returns `ALREADY_COMPLETED`.

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
| `share_url` | Full shareable URL. Null until processing completes successfully |

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

Fetches video metadata and streaming info for the player page.

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

**Triggered by**: Worker consumes the `ProcessVideo` task scheduled by
[UC-VID-002](#uc-vid-002) on successful upload completion.

Probes the file, transcodes it into adaptive HLS, and uploads the segments.
Segments are uploaded to storage progressively while the pipeline is still
running. The share token is written to the video row as soon as the low
tier's first segment and the master playlist are both in storage — the
viewer's share link becomes valid well before the full transcode finishes.
Once every tier has finished, the status flips to `PROCESSED`.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `video_id` | Yes | The video to process |

**Guards**
1. Video exists and status is `UPLOADED`
2. Original file exists in storage (validated implicitly by the probe)

**Mutations**
1. Atomically set `status = PROCESSING` (only if still `UPLOADED`). If not, skip — another worker claimed it.
2. Probe the file — confirm it's a valid, decodable video and capture stream info.
3. Start the transcode pipeline. Segments and per-tier playlists are uploaded to
   storage progressively as the pipeline produces them (not in a single batch
   at the end).
4. **Early share-link publish**: the first moment the low tier's first segment
   and the master playlist are both durable in storage, atomically write
   `share_token` to the video row (only if status is still `Processing`). This
   flips the share link to live while the rest of the transcode continues in
   the background. Viewers who open the link before all tiers finish get a
   playable low-quality stream that fills in with higher tiers as they land.
5. Wait for the pipeline to finish all tiers.
6. On success → atomically set `status = PROCESSED` (share token is already
   written; this step is purely the status flip). Step 4's write and this
   step's write are both guarded on the row still being in `Processing`, so
   a failure path that flipped the status in between (rare) cleanly wins.
7. Delete original upload from storage. Best-effort — a failure here leaves
   an orphan that the cleanup safety-net (UC-VID-006) collects.

On probe failure or transcode failure:
- Atomically set `status = FAILED` and schedule a `DeleteVideo` task
  ([UC-VID-007](#uc-vid-007)) in the same DB transaction. The task removes
  the original upload, any partial output in storage, and the video record.
- If the failure happens **before** step 4 (no share token issued yet), the
  uploader sees `FAILED` when polling — same behavior as before this change.
- If the failure happens **after** step 4 (share token already issued), the
  viewer holding the link briefly sees a playable stream that stops advancing,
  then `VIDEO_NOT_FOUND` once `DeleteVideo` runs. The share token is *not*
  "revoked" in any nuanced way — deletion removes the row and everyone's link
  stops working the same way. Accepted tradeoff: the race window is minutes at
  most, and the product is anonymous casual sharing where a short-lived broken
  link is less bad than waiting minutes longer for *every* link to appear.

**Why early publish is minimal, not a return to `PARTIAL`**: the 2026-04-07
change removed a previous "first segment" mechanism that introduced
`PARTIAL` and `INCOMPLETE` states in the enum, segment-counting callbacks,
and branching failure paths that tried to preserve half-finished output.
This change reintroduces the time-to-stream goal without any of that: the
enum is unchanged (`PROCESSING` still means "work in progress" and is also
now the state in which a video can already be watched via its share link),
there is no partial-preservation story on failure (we still delete everything
the same way), and the trigger is a single one-shot event from the transcoder
rather than a state machine the use case has to track. The entire surface
of the change is one extra atomic write during `Processing` and one earlier
moment at which the share link becomes visible to pollers.

**Output**: N/A (system process)

**Error Codes**: N/A (failures recorded on the video entity)

**Side Effects**

- On probe or transcode failure: schedules `DeleteVideo`
  ([UC-VID-007](#uc-vid-007)) in the same transaction as the `FAILED` status update.

**Idempotency**: Not safely retryable. Re-running after a successful first
attempt is a no-op (the initial `update_status_if(Uploaded → Processing)`
returns false). Re-running after a *failed* attempt is also a no-op for the
same reason — the row is already in `Processing` or `Failed`. The task is
configured with `max_retries = 1` so the task system does not retry; any
failure is terminal and the safety-net sweep ([UC-VID-006](#uc-vid-006))
collects the row 24 hours later. See
[task-catalog.md#processvideo](../task-system/task-catalog.md#processvideo).

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
  bulk-mark `status = FAILED`, then bulk-schedule `DeleteVideo` for the same set
- `FAILED` videos older than 24 hours → schedule `DeleteVideo`
- `PROCESSED` videos are kept indefinitely

**Why 24 hours on FAILED**: the primary failure paths
([UC-VID-002](#uc-vid-002) rejection,
[UC-VID-005](#uc-vid-005) transcode failure) already schedule `DeleteVideo`
immediately, in the same transaction as the `FAILED` transition. The sweep's
FAILED branch is a safety net for the rare case where a video ended up
`FAILED` without a `DeleteVideo` task (worker crashed between operations,
manual DB intervention, future code path forgetting to schedule). The 24-hour
window:
- Gives the primary `DeleteVideo` task time to retry and complete, so the
  sweep does not race it.
- Prevents the sweep from creating redundant task rows on every run for
  videos that already have an in-flight `DeleteVideo`.
- Gives operators a window to inspect failed videos before they are
  permanently removed.

**Why bulk-mark and bulk-schedule are separate statements**: the sweep is
itself a safety net, and a partial failure between the two statements is
recovered by the next sweep run (the now-FAILED videos are picked up via the
FAILED-older-than-24h branch). Strict per-video atomicity would require
expanding the transactional mutation API for negligible benefit, since the
sweep is eventually consistent within one cycle.

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
| `video_id` | Yes | The video to delete |

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
