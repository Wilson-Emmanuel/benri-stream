use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait StoragePort: Send + Sync {
    /// Generate a presigned URL the client can `PUT` directly to.
    ///
    /// `max_size_bytes` is documented as the upload limit but is **not**
    /// enforced by the URL itself — AWS PUT presigning has no
    /// content-length-range condition (only POST policies do). Size
    /// enforcement happens server-side in `complete_upload` via
    /// `head_object` once the upload completes. The parameter is kept
    /// on the trait so a POST-policy implementation could enforce it at
    /// the storage layer without changing callers.
    async fn generate_presigned_upload_url(
        &self,
        key: &str,
        content_type: &str,
        max_size_bytes: i64,
        expiry_secs: u64,
    ) -> Result<PresignedUpload, StorageError>;

    /// Generate a presigned URL for reading an object. Used by the
    /// transcoder so input files can be read without requiring the
    /// bucket to be publicly readable.
    async fn generate_presigned_download_url(
        &self,
        key: &str,
        expiry_secs: u64,
    ) -> Result<String, StorageError>;

    async fn head_object(&self, key: &str) -> Result<Option<ObjectMetadata>, StorageError>;

    async fn read_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, StorageError>;

    async fn upload_from_path(
        &self,
        local_path: &std::path::Path,
        key: &str,
        content_type: &str,
    ) -> Result<(), StorageError>;

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
