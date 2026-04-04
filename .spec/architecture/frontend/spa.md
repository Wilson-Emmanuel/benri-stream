# Frontend Architecture

Svelte SPA with two routes:

| Route | Page | What it does |
|-------|------|-------------|
| `/` | Upload | File drop zone, title input, upload progress, polling for link |
| `/v/{shareToken}` | Player | Fetches video metadata from API, plays HLS via hls.js |

---

## Upload Page Flow

1. User drops file → frontend validates (type, size, header check)
2. Calls `POST /api/videos/initiate` → gets presigned URL + video ID
3. Uploads directly to storage using presigned URL
4. Calls `POST /api/videos/{id}/complete`
5. Polls `GET /api/videos/{id}/status` until `shareUrl` appears
6. Shows the shareable link

---

## Player Page Flow

1. Extracts share token from URL
2. Calls `GET /api/videos/share/{shareToken}`
3. If `streamUrl` is present → initializes hls.js player
4. If `streamUrl` is null → shows loading, polls until available
