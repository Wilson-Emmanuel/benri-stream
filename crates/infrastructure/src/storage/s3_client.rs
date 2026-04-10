use async_trait::async_trait;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use chrono::Utc;
use std::time::Duration;

use domain::ports::storage::{ObjectMetadata, PresignedUpload, StorageError, StoragePort};

/// S3-compatible storage adapter that routes by key prefix to one of two
/// buckets:
///
/// - `uploads/...` → `upload_bucket` (private; worker reads via presigned GET)
/// - `videos/...`  → `output_bucket` (public-read via CDN; HLS output)
///
/// The two-bucket split gives independent access controls and lifecycle rules
/// for originals vs. public output. Domain code is unaware of the routing —
/// keys already carry their prefix.
pub struct S3StorageClient {
    client: Client,
    /// When set, used instead of `client` for signing browser-facing upload
    /// URLs. Required in docker-compose where the internal MinIO hostname is
    /// not browser-reachable. `None` is correct for real AWS and the worker.
    upload_presign_client: Option<Client>,
    upload_bucket: String,
    output_bucket: String,
    cdn_base_url: String,
}

const UPLOAD_PREFIX: &str = "uploads/";
const OUTPUT_PREFIX: &str = "videos/";

impl S3StorageClient {
    pub fn new(
        client: Client,
        upload_bucket: String,
        output_bucket: String,
        cdn_base_url: String,
    ) -> Self {
        Self {
            client,
            upload_presign_client: None,
            upload_bucket,
            output_bucket,
            cdn_base_url,
        }
    }

    /// Override the S3 client used to sign browser-facing upload URLs.
    pub fn with_upload_presign_client(mut self, client: Client) -> Self {
        self.upload_presign_client = Some(client);
        self
    }

    /// Returns the bucket for the given key. Panics on unrecognized prefixes —
    /// all callers produce `uploads/` or `videos/` keys by construction.
    fn bucket_for(&self, key: &str) -> &str {
        if key.starts_with(UPLOAD_PREFIX) {
            &self.upload_bucket
        } else if key.starts_with(OUTPUT_PREFIX) {
            &self.output_bucket
        } else {
            panic!(
                "S3StorageClient: key '{}' has no recognized prefix \
                 (expected '{}' or '{}')",
                key, UPLOAD_PREFIX, OUTPUT_PREFIX
            );
        }
    }
}

#[async_trait]
impl StoragePort for S3StorageClient {
    async fn generate_presigned_upload_url(
        &self,
        key: &str,
        content_type: &str,
        // AWS PUT presigning has no content-length-range condition (only POST
        // policies do). Size is enforced server-side via `head_object` after
        // upload. The parameter is on the trait so a POST-policy
        // implementation can enforce it at the storage layer.
        _max_size_bytes: i64,
        expiry_secs: u64,
    ) -> Result<PresignedUpload, StorageError> {
        tracing::info!(key, content_type, expiry_secs, "s3: generating presigned upload url");

        let config = PresigningConfig::expires_in(Duration::from_secs(expiry_secs))
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Use the override client so the URL's Host header matches a
        // browser-reachable endpoint; fall back to the main client otherwise.
        let signer = self.upload_presign_client.as_ref().unwrap_or(&self.client);
        let presigned = signer
            .put_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .content_type(content_type)
            .presigned(config)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(PresignedUpload {
            url: presigned.uri().to_string(),
            expires_at: Utc::now() + chrono::Duration::seconds(expiry_secs as i64),
        })
    }

    async fn generate_presigned_download_url(
        &self,
        key: &str,
        expiry_secs: u64,
    ) -> Result<String, StorageError> {
        tracing::info!(key, expiry_secs, "s3: generating presigned download url");

        let config = PresigningConfig::expires_in(Duration::from_secs(expiry_secs))
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Always use the main client — this URL is consumed by the worker's
        // GStreamer pipeline, which is inside the same network as MinIO.
        let presigned = self
            .client
            .get_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .presigned(config)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(presigned.uri().to_string())
    }

    async fn head_object(&self, key: &str) -> Result<Option<ObjectMetadata>, StorageError> {
        tracing::info!(key, "s3: head object");
        match self
            .client
            .head_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .send()
            .await
        {
            Ok(output) => Ok(Some(ObjectMetadata {
                size_bytes: output.content_length().unwrap_or(0),
                content_type: output.content_type().map(|s| s.to_string()),
            })),
            Err(e) => {
                let err = e.into_service_error();
                if err.is_not_found() {
                    Ok(None)
                } else {
                    Err(StorageError::Internal(err.to_string()))
                }
            }
        }
    }

    async fn read_range(&self, key: &str, start: u64, end: u64) -> Result<Vec<u8>, StorageError> {
        tracing::info!(key, start, end, "s3: reading object range");
        let output = self
            .client
            .get_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .range(format!("bytes={}-{}", start, end))
            .send()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        output
            .body
            .collect()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| StorageError::Internal(e.to_string()))
    }

    async fn upload_from_path(
        &self,
        local_path: &std::path::Path,
        key: &str,
        content_type: &str,
    ) -> Result<(), StorageError> {
        // Buffer the whole file and delegate to upload_bytes rather than
        // using ByteStream::from_path. The streaming path stalls ~30 s per
        // request against MinIO (any S3 that doesn't send `100-continue` for
        // chunk-signed streaming PUTs), while the buffered path completes the
        // same upload in <15 ms. HLS segments are a few MB each at most, so
        // the RSS cost is negligible. If segment sizes grow significantly,
        // multipart upload is the right answer — not streaming PUTs.
        tracing::info!(key, content_type, "s3: reading segment into memory for upload");
        let bytes = tokio::fs::read(local_path).await.map_err(|e| {
            StorageError::Internal(format!("failed to read {}: {}", local_path.display(), e))
        })?;
        self.upload_bytes(key, &bytes, content_type).await
    }

    async fn upload_bytes(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        tracing::info!(
            key,
            content_type,
            size = bytes.len(),
            "s3: uploading object from memory",
        );
        let body = aws_sdk_s3::primitives::ByteStream::from(bytes.to_vec());
        self.client
            .put_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_object(&self, key: &str) -> Result<(), StorageError> {
        tracing::info!(key, "s3: deleting object");
        self.client
            .delete_object()
            .bucket(self.bucket_for(key))
            .key(key)
            .send()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<(), StorageError> {
        tracing::info!(prefix, "s3: deleting prefix");
        let bucket = self.bucket_for(prefix);
        let mut token: Option<String> = None;
        loop {
            let mut req = self.client.list_objects_v2().bucket(bucket).prefix(prefix);
            if let Some(t) = &token {
                req = req.continuation_token(t);
            }
            let output = req.send().await.map_err(|e| StorageError::Internal(e.to_string()))?;

            // list_objects_v2 returns ≤1000 keys per page; delete_objects
            // accepts ≤1000 keys per call — one page maps to one delete.
            let to_delete: Vec<ObjectIdentifier> = output
                .contents()
                .iter()
                .filter_map(|obj| obj.key())
                .map(|k| {
                    ObjectIdentifier::builder()
                        .key(k)
                        .build()
                        .expect("ObjectIdentifier::build only fails on missing key")
                })
                .collect();

            if !to_delete.is_empty() {
                let delete = Delete::builder()
                    .set_objects(Some(to_delete))
                    .build()
                    .expect("Delete::build only fails on missing objects list");

                self.client
                    .delete_objects()
                    .bucket(bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
            }

            if output.is_truncated() == Some(true) {
                token = output.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }
        Ok(())
    }

    fn public_url(&self, key: &str) -> String {
        format!("{}/{}", self.cdn_base_url.trim_end_matches('/'), key)
    }
}
