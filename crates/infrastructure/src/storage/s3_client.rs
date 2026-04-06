use async_trait::async_trait;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::Client;
use chrono::Utc;
use std::time::Duration;

use domain::ports::storage::{ObjectMetadata, PresignedUpload, StorageError, StoragePort};

pub struct S3StorageClient {
    client: Client,
    bucket: String,
    cdn_base_url: String,
}

impl S3StorageClient {
    pub fn new(client: Client, bucket: String, cdn_base_url: String) -> Self {
        Self { client, bucket, cdn_base_url }
    }
}

#[async_trait]
impl StoragePort for S3StorageClient {
    async fn generate_presigned_upload_url(
        &self,
        key: &str,
        content_type: &str,
        _max_size_bytes: i64,
        expiry_secs: u64,
    ) -> Result<PresignedUpload, StorageError> {
        let config = PresigningConfig::expires_in(Duration::from_secs(expiry_secs))
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let presigned = self
            .client
            .put_object()
            .bucket(&self.bucket)
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

    async fn head_object(&self, key: &str) -> Result<Option<ObjectMetadata>, StorageError> {
        match self.client.head_object().bucket(&self.bucket).key(key).send().await {
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
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
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
        let body = aws_sdk_s3::primitives::ByteStream::from_path(local_path)
            .await
            .map_err(|e| StorageError::Internal(format!("failed to read {}: {}", local_path.display(), e)))?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn delete_object(&self, key: &str) -> Result<(), StorageError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<(), StorageError> {
        let mut token: Option<String> = None;
        loop {
            let mut req = self.client.list_objects_v2().bucket(&self.bucket).prefix(prefix);
            if let Some(t) = &token {
                req = req.continuation_token(t);
            }
            let output = req.send().await.map_err(|e| StorageError::Internal(e.to_string()))?;
            for obj in output.contents() {
                if let Some(key) = obj.key() {
                    self.delete_object(key).await?;
                }
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
