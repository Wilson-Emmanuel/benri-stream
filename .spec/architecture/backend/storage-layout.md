# Storage

S3-compatible object storage split across two buckets: a private upload
bucket for original files (read by the worker only) and a public-read
output bucket for HLS manifests and segments (fronted by the CDN).

---

## Object Layout

Two buckets, one prefix each. Keys still carry the prefix even though
the bucket name already implies it — the prefix is what the storage
adapter routes on, so domain code stays unaware of the bucket split.

```
benri-uploads/                       <- private bucket
  uploads/
    {video_id}/original.{ext}        <- deleted after processing

benri-stream/                        <- public-read bucket (CDN origin)
  videos/
    {video_id}/
      master.m3u8                    <- HLS master playlist (multi-variant)
      low/
        playlist.m3u8                <- per-tier playlist
        segment_000.ts               <- MPEG-TS segments
        segment_001.ts
        ...
      medium/
        playlist.m3u8
        ...
      high/
        playlist.m3u8
        ...
```

---

## Port and Implementation

The domain defines what the system needs from storage. Infrastructure provides the
S3 implementation.

**Port trait** (domain):
```rust
// crates/domain/src/ports/storage.rs
pub trait StoragePort: Send + Sync {
    async fn generate_presigned_upload_url(...) -> Result<PresignedUpload, StorageError>;
    async fn head_object(&self, key: &str) -> Result<Option<ObjectMetadata>, StorageError>;
    async fn read_range(&self, key: &str, start: u64, end: u64) -> Result<Vec<u8>, StorageError>;
    async fn upload_from_stream(...) -> Result<(), StorageError>;
    async fn delete_object(&self, key: &str) -> Result<(), StorageError>;
    async fn delete_prefix(&self, prefix: &str) -> Result<(), StorageError>;
    fn public_url(&self, key: &str) -> String;
}
```

**Implementation** (infrastructure):
```rust
// crates/infrastructure/src/storage/s3_client.rs
pub struct S3StorageClient {
    client: aws_sdk_s3::Client,
    upload_bucket: String,   // private; uploads/...
    output_bucket: String,   // public-read; videos/...
    cdn_base_url: String,
}
impl StoragePort for S3StorageClient { ... }
```

The adapter holds both buckets and routes per call by inspecting the
key prefix (`uploads/` → upload bucket, `videos/` → output bucket).
A `bucket_for(key)` helper does the dispatch and panics on unknown
prefixes (programming bug, not a runtime condition).

---

## Configuration

The adapter holds two bucket names and routes by key prefix:
`uploads/...` keys go to the upload bucket, `videos/...` keys go to
the output bucket. The split keeps the public-read surface (HLS output)
fully separated from the private surface (original uploads), with
independent access policies, lifecycle rules, and metrics.

| Config | Env var | Description |
|--------|---------|-------------|
| Upload bucket | `S3_UPLOAD_BUCKET` | Private bucket for `uploads/{id}/...`. Worker reads via short-lived presigned GET URLs. |
| Output bucket | `S3_OUTPUT_BUCKET` | Public-read bucket for `videos/{id}/...`. Fronted by the CDN. |
| Region | `S3_REGION` | S3 region |
| Endpoint | `S3_ENDPOINT` | Custom endpoint for S3-compatible providers (MinIO, etc.) |
| CDN base URL | `CDN_BASE_URL` | Prefix for public URLs resolved by `public_url()` |

Configured in `crates/infrastructure/src/config.rs`. Read from environment at startup
in `api/src/main.rs` and `worker/src/main.rs`.

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| `StoragePort` trait | `domain` | `src/ports/storage.rs` |
| `StorageError`, `PresignedUpload`, `ObjectMetadata` | `domain` | `src/ports/storage.rs` |
| `S3StorageClient` implementation | `infrastructure` | `src/storage/s3_client.rs` |
| Config (upload + output buckets, region, endpoint, CDN URL) | `infrastructure` | `src/config.rs` |
| Wiring (construct client, pass to use cases) | `api`, `worker` | `src/main.rs` |
