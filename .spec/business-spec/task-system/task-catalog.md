# Task Catalog

All background tasks in benri-stream. For how the task system works (entity, lifecycle,
domain types, worker design), see
[architecture/backend/task-system.md](../../architecture/backend/task-system.md).

---

## Video Tasks

| Task | Trigger | Use Case | Notes |
|------|---------|----------|-------|
| Process Video | Video status set to `UPLOADED` | [UC-VID-005](../video/video.md#uc-vid-005) | Atomic claim via `UPLOADED → PROCESSING` |
| Delete Video | Scheduled by [UC-VID-002](../video/video.md#uc-vid-002) (rejection), [UC-VID-005](../video/video.md#uc-vid-005) (failure), [UC-VID-006](../video/video.md#uc-vid-006) (safety-net sweep) | [UC-VID-007](../video/video.md#uc-vid-007) | Per-video. Ordering key `video_delete:{id}` with dedup-by-default. Retries with exponential backoff; dead-letters on persistent failure. Single delete path for all video removal. |
| Cleanup Stale Videos | Daily schedule (system task) | [UC-VID-006](../video/video.md#uc-vid-006) | Safety-net sweep. Enumerates qualifying videos and schedules `Delete Video` tasks — performs no direct deletion. Cleans `PENDING_UPLOAD` (24h), stuck `UPLOADED`/`PROCESSING` (24h), `FAILED` (24h). |
