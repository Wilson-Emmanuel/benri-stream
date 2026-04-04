# Task Catalog

All background tasks in benri-stream. For how the task system works (entity, lifecycle,
domain types, worker design), see
[architecture/backend/task-system.md](../../architecture/backend/task-system.md).

---

## Video Tasks

| Task | Trigger | Use Case | Notes |
|------|---------|----------|-------|
| Process Video | Video status set to `UPLOADED` | [UC-VID-005](../video/video.md#uc-vid-005) | Atomic claim via `UPLOADED → PROCESSING` |
| Cleanup Stale Videos | Daily schedule | [UC-VID-006](../video/video.md#uc-vid-006) | Cleans `PENDING_UPLOAD` (24h), stuck `UPLOADED`/`PROCESSING` (24h), old `FAILED` (30d) |
