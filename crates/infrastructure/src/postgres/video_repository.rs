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
    async fn insert(&self, video: &Video) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO videos (id, share_token, title, format, status, upload_key, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(video.id.0)
        .bind(&video.share_token)
        .bind(&video.title)
        .bind(video.format.as_str())
        .bind(video.status.as_str())
        .bind(&video.upload_key)
        .bind(video.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }

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

    async fn update_status_if(
        &self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError> {
        let result = sqlx::query(
            "UPDATE videos SET status = $3 WHERE id = $1 AND status = $2",
        )
        .bind(id.0)
        .bind(expected.as_str())
        .bind(new_status.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_share_token(&self, id: &VideoId, token: &str) -> Result<(), RepositoryError> {
        sqlx::query("UPDATE videos SET share_token = $2 WHERE id = $1")
            .bind(id.0)
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
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

    async fn delete(&self, id: &VideoId) -> Result<(), RepositoryError> {
        sqlx::query("DELETE FROM videos WHERE id = $1")
            .bind(id.0)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }
}
