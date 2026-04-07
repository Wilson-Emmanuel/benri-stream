//! Helpers for constructing the concrete clients (Postgres, S3, Redis)
//! that the api / worker binaries wire into the domain ports at startup.
//!
//! Centralizing client construction here keeps each composition root
//! (`api/src/main.rs`, `worker/src/main.rs`) free of direct SDK
//! dependencies — they only need `infrastructure`, `application`, and
//! `domain` to bootstrap.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::AppConfig;

/// Connect to Postgres with a bounded connection pool. The pool size
/// is capped to avoid exhausting the database's `max_connections` when
/// multiple worker / api replicas run concurrently.
pub async fn create_pg_pool(
    database_url: &str,
    max_connections: u32,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
}

/// Build an `aws_sdk_s3::Client` from the configured region and (for
/// MinIO / other S3-compatible providers) custom endpoint URL.
pub async fn create_s3_client(config: &AppConfig) -> aws_sdk_s3::Client {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.s3_region.clone()));
    let aws_config = if let Some(endpoint) = &config.s3_endpoint {
        aws_config.endpoint_url(endpoint).load().await
    } else {
        aws_config.load().await
    };
    aws_sdk_s3::Client::new(&aws_config)
}

/// Open a Redis client (multiplexed connections are negotiated lazily
/// per call inside the publisher / consumer / lock adapters).
pub fn create_redis_client(redis_url: &str) -> Result<redis::Client, redis::RedisError> {
    redis::Client::open(redis_url)
}
