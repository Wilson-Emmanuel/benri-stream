# Storage

S3-compatible object storage for uploaded files, transcoded HLS output, and presigned
URL uploads.

---

## Object Layout

```
uploads/
  {video_id}/original.{ext}         <- temp, deleted after processing

videos/
  {video_id}/
    master.m3u8                      <- HLS master playlist (multi-variant)
    low/
      playlist.m3u8                  <- per-tier playlist
      segment_000.m4s               <- fragmented MP4 segments
      segment_001.m4s
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
pub struct S3StorageClient { client: aws_sdk_s3::Client, bucket: String, cdn_base_url: String }
impl StoragePort for S3StorageClient { ... }
```

---

## Configuration

| Config | Env var | Description |
|--------|---------|-------------|
| Bucket name | `S3_BUCKET` | Where all objects live |
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
| Config (bucket, region, endpoint, CDN URL) | `infrastructure` | `src/config.rs` |
| Wiring (construct client, pass to use cases) | `api`, `worker` | `src/main.rs` |
