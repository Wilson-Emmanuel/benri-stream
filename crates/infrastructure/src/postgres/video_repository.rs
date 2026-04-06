use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use domain::ports::video::{RepositoryError, VideoRepository};
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

pub struct PostgresVideoRepository {
    pool: PgPool,
}

impl PostgresVideoRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_video(row: sqlx::postgres::PgRow) -> Video {
    Video {
        id: VideoId(row.get("id")),
        share_token: row.get("share_token"),
        title: row.get("title"),
        format: VideoFormat::from_str(row.get("format")).unwrap_or(VideoFormat::Mp4),
        status: VideoStatus::from_str(row.get("status")),
        upload_key: row.get("upload_key"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl VideoRepository for PostgresVideoRepository {
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError> {
        sqlx::query("SELECT * FROM videos WHERE id = $1")
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(row_to_video))
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError> {
        sqlx::query("SELECT * FROM videos WHERE share_token = $1")
            .bind(token)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(row_to_video))
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_stale(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError> {
        sqlx::query(
            "SELECT * FROM videos WHERE status IN ('PENDING_UPLOAD', 'UPLOADED', 'PROCESSING')
             AND created_at < $1",
        )
        .bind(before)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(row_to_video).collect())
        .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_failed_before(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError> {
        sqlx::query("SELECT * FROM videos WHERE status = 'FAILED' AND created_at < $1")
            .bind(before)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(row_to_video).collect())
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }
}
