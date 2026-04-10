# Storage

S3-compatible object storage. Two buckets: a private upload bucket for originals and a public-read output bucket for HLS output (fronted by CDN).

---

## Object Layout

```
benri-uploads/                       <- private bucket
  uploads/{video_id}/original.{ext}  <- deleted after processing

benri-stream/                        <- public-read bucket (CDN origin)
  videos/{video_id}/
    master.m3u8
    low/playlist.m3u8, segment_000.ts, ...
    medium/playlist.m3u8, ...
    high/playlist.m3u8, ...
```

---

## Two-Bucket Routing

The `S3StorageClient` holds both bucket names and routes by key prefix: `uploads/...` keys go to the upload bucket, `videos/...` keys go to the output bucket. A `bucket_for(key)` helper does the dispatch. Domain code is unaware of the bucket split — keys already carry their prefix.

This keeps the public-read surface (HLS output) fully separated from the private surface (original uploads), with independent access policies and lifecycle rules.

---

## Port and Implementation

**Port** — `StoragePort` trait in `domain/src/ports/storage.rs`. Methods: `generate_presigned_upload_url`, `generate_presigned_download_url`, `head_object`, `read_range`, `upload_from_path`, `upload_bytes`, `delete_object`, `delete_prefix`, `public_url`.

**Implementation** — `S3StorageClient` in `infrastructure/src/storage/s3_client.rs`. Supports an optional `upload_presign_client` for docker-compose environments where the internal MinIO hostname is not browser-reachable.

Upload uses in-memory buffering (`tokio::fs::read` into `Vec<u8>`, then `ByteStream::from`). The streaming `ByteStream::from_path` stalls ~30s per request against MinIO; the buffered path completes in <15ms. HLS segments are a few MB, so RSS cost is negligible.

---

## Configuration

| Env var | Description |
|---------|-------------|
| `S3_UPLOAD_BUCKET` | Private bucket for `uploads/{id}/...` |
| `S3_OUTPUT_BUCKET` | Public-read bucket for `videos/{id}/...`, fronted by CDN |
| `S3_REGION` | S3 region |
| `S3_ENDPOINT` | Custom endpoint for S3-compatible providers (MinIO, etc.) |
| `S3_PUBLIC_ENDPOINT` | Endpoint for browser-facing presigned URLs (docker-compose) |
| `CDN_BASE_URL` | Prefix for public URLs |
