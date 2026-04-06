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

---

## ProcessVideo

Transcodes an uploaded video into HLS segments.

| | |
|---|---|
| **Metadata type name** | `ProcessVideoTaskMetadata` |
| **Fields** | `video_id: VideoId` |
| **Use case** | [UC-VID-005 Process Video](../video/video.md#uc-vid-005) |
| **Ordering key** | `video_process:{video_id}` — dedup-by-default and sequential. Prevents two concurrent process attempts on the same video. |
| **Max retries** | `5` |
| **Retry base delay** | `60 seconds` |
| **Execution interval** | N/A (one-shot) |
| **Processing timeout** | `30 minutes` — transcoding a 1 GB video can take a while. |
| **System task** | `false` |

**Failure model**: handler maps `VideoNotFound` → `PermanentFailure` (dead letter),
any other error → `RetryableFailure`. The use case itself atomically transitions
to `FAILED` and schedules a `DeleteVideo` task on probe / zero-segment failures,
so the `RetryableFailure` path only fires on infrastructure issues (DB down,
etc.) that should be retried.

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
