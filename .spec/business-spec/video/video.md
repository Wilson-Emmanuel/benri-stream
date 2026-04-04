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

---

## Definitions

### Attributes

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|
| `id` | Unique identifier | No | Internal system identifier |
| `shareToken` | Text (21 chars) | Yes | Unique, unguessable token for the shareable link. Null until first segment is produced. URL-safe |
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
| `PARTIAL` | Watchable from the beginning, later parts still being processed |
| `PROCESSED` | Fully processed and watchable at all quality levels |
| `INCOMPLETE` | Processing failed partway. Whatever succeeded is streamable — video ends earlier |
| `FAILED` | No segments produced. No shareable link generated |

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

**Side Effects**: N/A (worker picks up `UPLOADED` videos independently)

**Idempotency**: Not idempotent — calling again after completion returns `ALPROCESSED_COMPLETED`.

---

### Get Video Status {#UC-VID-003}

**Actor**: Anyone (the uploader polls this after completing upload)

**Triggered by**: REST: `GET /api/videos/{id}/status`

Polling endpoint. Returns current status and the shareable link once it's available
(after first segment is produced).

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
| `shareUrl` | Full shareable URL. Null until first segment is produced |

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

**Triggered by**: Worker polls for videos with `status = UPLOADED`

Probes the file, transcodes segment by segment, and writes output directly to storage.
The shareable link is generated after the first segment succeeds.

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `videoId` | Yes | The video to process |

**Guards**
1. Video exists and status is `UPLOADED`
2. Original file exists in storage

**Mutations** (progressive)
1. Atomically set `status = PROCESSING` (only if still `UPLOADED`). If not, skip — another worker claimed it.
2. Probe the file — confirm it's a valid, processable video
3. Transcode first segment at all three quality levels, writing output directly to storage
4. On first segment success → generate `shareToken`, set `status = PARTIAL`
5. Continue transcoding remaining segments directly to storage
6. On completion → set `status = PROCESSED`
7. Delete original file from storage

On probe failure:
- Set `status = FAILED`
- Delete original file from storage
- No share token generated — uploader sees FAILED when polling

On segment failure:
- If some segments already succeeded: set `status = INCOMPLETE`,
  keep successful segments. Video is streamable up to that point.
- If no segments succeeded: set `status = FAILED`. No share token generated.

**Output**: N/A (system process)

**Error Codes**: N/A (failures recorded on the video entity)

**Side Effects**: N/A

**Idempotency**: Idempotent — re-running on PROCESSED is a no-op.

---

### Cleanup Stale Videos (System) {#UC-VID-006}

**Actor**: System — not user-facing

**Triggered by**: Worker runs daily on schedule

Cleans up stuck or expired video records and orphaned files.

**Input**: N/A

**Guards**: N/A

**Mutations**
- `PENDING_UPLOAD` older than 24 hours → delete record + any storage files
- `UPLOADED` or `PROCESSING` older than 24 hours with no progress → set `status = FAILED`,
  delete original file
- `FAILED` videos older than 30 days → delete record + any remaining files
- `INCOMPLETE` videos are NOT cleaned up — they have working segments and are streamable

**Output**: N/A

**Error Codes**: N/A

**Side Effects**: N/A

**Idempotency**: Idempotent — re-running deletes only newly qualifying records.

---

## Limits and Quotas

| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max upload file size | 1 GB | Presigned URL policy (storage-enforced) + client-side check + guard on complete |
| Max title length | 100 chars | Guard on initiate |
| Quality levels | 3 (low, medium, high) | Processing pipeline |
| Stale upload/processing timeout | 24 hours | Cleanup task |
| Failed video retention | 30 days | Cleanup task |
| Share token length | 21 chars | Generated on first segment success |
