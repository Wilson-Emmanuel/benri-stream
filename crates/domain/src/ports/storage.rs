use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait StoragePort: Send + Sync {
    async fn generate_presigned_upload_url(
        &self,
        key: &str,
        content_type: &str,
        max_size_bytes: i64,
        expiry_secs: u64,
    ) -> Result<PresignedUpload, StorageError>;

    async fn head_object(&self, key: &str) -> Result<Option<ObjectMetadata>, StorageError>;

    async fn read_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, StorageError>;

    async fn delete_object(&self, key: &str) -> Result<(), StorageError>;

    async fn delete_prefix(&self, prefix: &str) -> Result<(), StorageError>;

    fn public_url(&self, key: &str) -> String;
}

#[derive(Debug, Clone)]
pub struct PresignedUpload {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    pub size_bytes: i64,
    pub content_type: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("storage error: {0}")]
    Internal(String),
    #[error("object not found: {0}")]
    NotFound(String),
}
