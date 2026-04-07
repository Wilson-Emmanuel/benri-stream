use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use domain::ports::error::RepositoryError;
use domain::ports::video::VideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

pub struct PostgresVideoRepository {
    pool: PgPool,
}

impl PostgresVideoRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Selected columns for `videos`. Listed once so the SELECTs and the row
/// mapper can't drift.
const VIDEO_COLUMNS: &str =
    "id, share_token, title, format, status, upload_key, created_at";

/// Map a `videos` row into the domain entity. Panics on unknown enum
/// values — those indicate corrupt DB state or a forgotten migration,
/// not a runtime condition the application can recover from.
fn row_to_video(row: sqlx::postgres::PgRow) -> Video {
    let format_str: &str = row.get("format");
    let status_str: &str = row.get("status");
    Video {
        id: VideoId(row.get("id")),
        share_token: row.get("share_token"),
        title: row.get("title"),
        format: VideoFormat::from_str(format_str)
            .unwrap_or_else(|| panic!("unknown VideoFormat in DB row: '{}'", format_str)),
        status: VideoStatus::from_str(status_str)
            .unwrap_or_else(|| panic!("unknown VideoStatus in DB row: '{}'", status_str)),
        upload_key: row.get("upload_key"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl VideoRepository for PostgresVideoRepository {
    async fn find_by_id(&self, id: &VideoId) -> Result<Option<Video>, RepositoryError> {
        sqlx::query(&format!("SELECT {VIDEO_COLUMNS} FROM videos WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(row_to_video))
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_by_share_token(&self, token: &str) -> Result<Option<Video>, RepositoryError> {
        sqlx::query(&format!("SELECT {VIDEO_COLUMNS} FROM videos WHERE share_token = $1"))
            .bind(token)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(row_to_video))
            .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_stale(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError> {
        sqlx::query(&format!(
            "SELECT {VIDEO_COLUMNS} FROM videos
             WHERE status IN ('PENDING_UPLOAD', 'UPLOADED', 'PROCESSING')
               AND created_at < $1"
        ))
        .bind(before)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(row_to_video).collect())
        .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn find_failed_before(&self, before: DateTime<Utc>) -> Result<Vec<Video>, RepositoryError> {
        sqlx::query(&format!(
            "SELECT {VIDEO_COLUMNS} FROM videos WHERE status = 'FAILED' AND created_at < $1"
        ))
        .bind(before)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(row_to_video).collect())
        .map_err(|e| RepositoryError::Database(e.to_string()))
    }

    async fn insert(&self, video: &Video) -> Result<(), RepositoryError> {
        tracing::info!(video_id = %video.id, status = ?video.status, "db: inserting video");
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

    async fn update_status_if(
        &self,
        id: &VideoId,
        expected: VideoStatus,
        new_status: VideoStatus,
    ) -> Result<bool, RepositoryError> {
        tracing::info!(
            video_id = %id,
            expected = ?expected,
            new_status = ?new_status,
            "db: conditional video status update",
        );
        let result =
            sqlx::query("UPDATE videos SET status = $3 WHERE id = $1 AND status = $2")
                .bind(id.0)
                .bind(expected.as_str())
                .bind(new_status.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn mark_processed(
        &self,
        id: &VideoId,
        share_token: &str,
    ) -> Result<bool, RepositoryError> {
        tracing::info!(video_id = %id, "db: marking video processed");
        let result = sqlx::query(
            "UPDATE videos SET share_token = $2, status = 'PROCESSED'
             WHERE id = $1 AND status = 'PROCESSING'",
        )
        .bind(id.0)
        .bind(share_token)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete(&self, id: &VideoId) -> Result<(), RepositoryError> {
        tracing::info!(video_id = %id, "db: deleting video");
        sqlx::query("DELETE FROM videos WHERE id = $1")
            .bind(id.0)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }

    async fn bulk_mark_failed(
        &self,
        ids: &[VideoId],
        from_statuses: &[VideoStatus],
    ) -> Result<(), RepositoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        tracing::info!(
            count = ids.len(),
            from = ?from_statuses,
            "db: bulk marking videos failed",
        );
        let id_uuids: Vec<uuid::Uuid> = ids.iter().map(|v| v.0).collect();
        let from_strs: Vec<&'static str> =
            from_statuses.iter().map(|s| s.as_str()).collect();

        sqlx::query(
            "UPDATE videos SET status = 'FAILED'
             WHERE id = ANY($1) AND status = ANY($2)",
        )
        .bind(&id_uuids)
        .bind(&from_strs)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Database(e.to_string()))?;
        Ok(())
    }
}
