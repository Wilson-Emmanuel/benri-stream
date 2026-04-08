use async_trait::async_trait;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use chrono::Utc;
use std::time::Duration;

use domain::ports::storage::{ObjectMetadata, PresignedUpload, StorageError, StoragePort};

/// S3-compatible storage adapter routing by key prefix to one of two
/// buckets:
///
/// - `uploads/...` → `upload_bucket` (private; only the worker reads
///   originals via short-lived presigned GET URLs)
/// - `videos/...`  → `output_bucket` (public-read via the CDN; HLS
///   manifests and segments)
///
/// The split is enforced at the bucket level rather than via prefix
/// policies on a single bucket so the public surface and the private
/// surface have independent access controls, lifecycle rules, metrics,
/// and storage classes. Domain code is unaware of the split — keys
/// already carry their prefix, so the adapter routes transparently.
pub struct S3StorageClient {
    client: Client,
    /// Optional second client used only when signing the upload URL
    /// returned to browsers. When `None`, `client` is used for all
    /// operations — correct for real AWS S3 and for the worker, which
    /// never presigns upload URLs for browsers. When `Some`, the
    /// override is used only for `generate_presigned_upload_url` so
    /// the signed `Host` header matches the browser-reachable endpoint
    /// (docker-compose + MinIO case — the internal `minio:9000` host
    /// is not browser-reachable).
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

    /// Override the S3 client used for signing browser-facing upload
    /// URLs. Only the api ever needs this — see the
    /// `upload_presign_client` field docs for context.
    pub fn with_upload_presign_client(mut self, client: Client) -> Self {
        self.upload_presign_client = Some(client);
        self
    }

    /// Pick the right bucket for a given storage key. Panics on
    /// unknown prefixes — every caller in the codebase produces keys
    /// under `uploads/` or `videos/`, and an unknown prefix indicates
    /// a programming bug, not a runtime condition.
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
        // Not enforced in the URL: AWS PUT presigning has no
        // content-length-range condition (only POST policies do).
        // Size is enforced server-side in `complete_upload` via
        // `head_object` once the upload completes. The parameter is
        // kept on the trait so a POST-policy implementation could
        // enforce it at the storage layer without changing callers.
        _max_size_bytes: i64,
        expiry_secs: u64,
    ) -> Result<PresignedUpload, StorageError> {
        tracing::info!(key, content_type, expiry_secs, "s3: generating presigned upload url");

        let config = PresigningConfig::expires_in(Duration::from_secs(expiry_secs))
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        // Use the dedicated upload-presign client if configured so
        // the URL's Host header matches a browser-reachable endpoint.
        // Falls back to the main client when no override is set.
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

        // Always signed with the main client (never the
        // browser-facing override): this URL is consumed by the
        // worker's GStreamer pipeline inside the same docker network
        // as MinIO, so the backend endpoint is correct here.
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
        tracing::info!(key, content_type, "s3: uploading object from path");
        let body = aws_sdk_s3::primitives::ByteStream::from_path(local_path)
            .await
            .map_err(|e| StorageError::Internal(format!("failed to read {}: {}", local_path.display(), e)))?;

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

            // Batch-delete this page in one request instead of N
            // delete_object calls. S3 list_objects_v2 returns ≤1000 keys
            // per page and delete_objects accepts ≤1000 keys per request,
            // so one page = one delete request.
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
