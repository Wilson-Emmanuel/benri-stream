# Frontend Architecture

Svelte SPA with two routes:

| Route | Page | Purpose |
|-------|------|---------|
| `/` | Upload | File drop, title input, upload progress, poll for share link |
| `/v/{shareToken}` | Player | Fetch video metadata, play HLS via hls.js |

---

## Upload Flow

1. User drops file → frontend validates (type, size, header check)
2. `POST /api/videos/initiate` → presigned URL + video ID
3. Upload directly to storage via presigned URL
4. `POST /api/videos/{id}/complete`
5. Poll `GET /api/videos/{id}/status` until `shareUrl` appears
6. Display shareable link

## Player Flow

1. Extract share token from URL
2. `GET /api/videos/share/{shareToken}`
3. If `streamUrl` present → initialize hls.js player
4. If `streamUrl` null → poll until available

---

## File Locations

| What | Where |
|------|-------|
| Upload page | `frontend/src/routes/+page.svelte` |
| Player page | `frontend/src/routes/v/[shareToken]/+page.svelte` |
| API client | `frontend/src/lib/api.js` |
| HLS player config | Inside player page (`hls.js` import + config) |
| New route | Add `frontend/src/routes/<path>/+page.svelte` |
