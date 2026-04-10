# Task Catalog

All background tasks in benri-stream. This catalog is authoritative -- implementation must match exactly. For spec format, see [SPEC_GUIDE.md](../../SPEC_GUIDE.md). For runtime architecture, see [task-system.md](../../architecture/backend/task-system.md).

---

## ProcessVideo

Transcodes an uploaded video into HLS segments and publishes the share link early (when the low tier's first segment and master playlist land in storage). See [UC-VID-005](../video/video.md#uc-vid-005).

| | |
|---|---|
| **Metadata type name** | `ProcessVideoTaskMetadata` |
| **Fields** | `video_id: VideoId` |
| **Use case** | [UC-VID-005 Process Video](../video/video.md#uc-vid-005) |
| **Ordering key** | `video_process:{video_id}` |
| **Max retries** | `1` |
| **Retry base delay** | `60 seconds` |
| **Execution interval** | N/A (one-shot) |
| **Processing timeout** | `2 hours` |
| **System task** | `false` |

**Failure model**: `VideoNotFound` -> `PermanentFailure`. Any other error -> `RetryableFailure`. With `max_retries = 1`, that single retry converts to dead letter. The use case itself transitions the video to `FAILED` and schedules `DeleteVideo` on probe/transcode failures, so the dead-letter entry is for operator visibility.

---

## DeleteVideo

Single delete path for a video's storage objects and database record. See [UC-VID-007](../video/video.md#uc-vid-007).

| | |
|---|---|
| **Metadata type name** | `DeleteVideoTaskMetadata` |
| **Fields** | `video_id: VideoId` |
| **Use case** | [UC-VID-007 Delete Video](../video/video.md#uc-vid-007) |
| **Ordering key** | `video_delete:{video_id}` |
| **Max retries** | `5` |
| **Retry base delay** | `60 seconds` |
| **Execution interval** | N/A (one-shot) |
| **Processing timeout** | `5 minutes` |
| **System task** | `false` |

**Failure model**: `VideoNotFound` -> `Skip` (already deleted). Any other error -> `RetryableFailure`.

---

## CleanupStaleVideos

Daily safety-net sweep. Enumerates stale videos and schedules `DeleteVideo` tasks. No direct deletion. See [UC-VID-006](../video/video.md#uc-vid-006).

| | |
|---|---|
| **Metadata type name** | `CleanupStaleVideosTaskMetadata` |
| **Fields** | None (unit struct) |
| **Use case** | [UC-VID-006 Cleanup Stale Videos](../video/video.md#uc-vid-006) |
| **Ordering key** | `cleanup_stale_videos` (constant -- single instance system-wide) |
| **Max retries** | `3` |
| **Retry base delay** | `5 minutes` |
| **Execution interval** | `24 hours` |
| **Processing timeout** | `30 minutes` |
| **System task** | `true` |

**Failure model**: Any error -> `RetryableFailure`. Per-video scheduling failures inside the sweep are logged but do not fail the outer task.
