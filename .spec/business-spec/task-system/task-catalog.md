# Task Catalog

All background tasks in benri-stream. This catalog is **authoritative** — the
metadata fields and scheduling config listed here must match the implementation
exactly. For the spec format itself and the workflow for adding a new task
type, see [SPEC_GUIDE.md](../SPEC_GUIDE.md). For how the task system works at
runtime (entity, lifecycle, worker design), see
[architecture/backend/task-system.md](../../architecture/backend/task-system.md).

## Changelog

| Date | Change | Author |
|------|--------|--------|
| 2026-04-06 | Add ProcessVideo and CleanupStaleVideos. Move workflow guidance to SPEC_GUIDE. | Wilson |
| 2026-04-06 | Initial DeleteVideo entry | Wilson |
| 2026-04-09 | ProcessVideo: bump `processing_timeout` 30 min → 2 h to cover 1 GB source on CPU-only worker; drop `max_retries` 5 → 1 because our failure modes (bad source, hardware exhaustion) aren't improved by retry and the use-case claim guard makes a second attempt a no-op anyway. | Wilson |

---

## ProcessVideo

Transcodes an uploaded video into HLS segments. Also publishes the share
link early — the moment the low tier's first segment and the master
playlist are durable in storage — so viewers see a playable link well
before the full transcode finishes. See
[UC-VID-005](../video/video.md#uc-vid-005) for the full lifecycle.

| | |
|---|---|
| **Metadata type name** | `ProcessVideoTaskMetadata` |
| **Fields** | `video_id: VideoId` |
| **Use case** | [UC-VID-005 Process Video](../video/video.md#uc-vid-005) |
| **Ordering key** | `video_process:{video_id}` — dedup-by-default and sequential. Prevents two concurrent process attempts on the same video. |
| **Max retries** | `1` — one attempt, then dead letter. See failure model below. |
| **Retry base delay** | `60 seconds` — applies only to the single retry permitted by `max_retries`. |
| **Execution interval** | N/A (one-shot) |
| **Processing timeout** | `2 hours` — sized to fit a 1 GB source through three CPU-only x264 `ultrafast` tiers with a ~2× safety margin. The previous 30-minute value was hit by small real-world videos on dev hardware. |
| **System task** | `false` |

**Failure model**: handler maps `VideoNotFound` → `PermanentFailure` (dead
letter), any other error → `RetryableFailure`. With `max_retries = 1` that
retryable failure converts to dead letter after the single permitted retry.
The use case itself atomically transitions to `FAILED` and schedules a
`DeleteVideo` task on probe / transcode failures, so by the time the task
system sees a `RetryableFailure` the video row has already been taken care
of; the dead-letter entry is purely for operator visibility.

**Why so few retries**: our meaningful failure modes are (a) bad source
file (not retryable — the file won't get better), (b) worker resource
exhaustion mid-transcode (retrying on the same worker won't fix it), and
(c) the use case's `Uploaded → Processing` claim already ran on the first
attempt, so a retry finds the row in `Processing` and no-ops cleanly. One
attempt is honest about all three.

---

## DeleteVideo

Single delete path for a video's storage objects and database record
(UC-VID-007). Dedup-by-default ensures only one active task per video at any
time.

| | |
|---|---|
| **Metadata type name** | `DeleteVideoTaskMetadata` |
| **Fields** | `video_id: VideoId` |
| **Use case** | [UC-VID-007 Delete Video](../video/video.md#uc-vid-007) |
| **Ordering key** | `video_delete:{video_id}` — dedup-by-default. Repeated schedule calls for the same video while a task is active return the existing task. |
| **Max retries** | `5` |
| **Retry base delay** | `60 seconds` |
| **Execution interval** | N/A (one-shot) |
| **Processing timeout** | `5 minutes` — three S3 / DB operations. |
| **System task** | `false` |

**Failure model**: handler maps `VideoNotFound` → `Skip` (already deleted is a
no-op), any other error → `RetryableFailure`.

---

## CleanupStaleVideos

Daily safety-net sweep (UC-VID-006). Enumerates videos in stale states and
schedules `DeleteVideo` tasks. Performs no direct storage or database deletion —
all deletion flows through `DeleteVideo`.

| | |
|---|---|
| **Metadata type name** | `CleanupStaleVideosTaskMetadata` |
| **Fields** | None (unit struct) |
| **Use case** | [UC-VID-006 Cleanup Stale Videos](../video/video.md#uc-vid-006) |
| **Ordering key** | `cleanup_stale_videos` (constant) — only one active instance system-wide. |
| **Max retries** | `3` |
| **Retry base delay** | `5 minutes` |
| **Execution interval** | `24 hours` — runs once a day on success. |
| **Processing timeout** | `30 minutes` — may touch many rows on a large dataset. |
| **System task** | `true` |

**Failure model**: handler maps any error → `RetryableFailure`. Per-video
scheduling failures inside the sweep are logged and counted but do not fail
the outer task — the next sweep (or a direct-schedule path) will pick them up.
