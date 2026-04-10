use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::AppConfig;

/// Connect to Postgres with a bounded connection pool.
pub async fn create_pg_pool(
    database_url: &str,
    max_connections: u32,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
}

/// Build an `aws_sdk_s3::Client` from the configured region and optional
/// custom endpoint URL (for MinIO or other S3-compatible providers).
pub async fn create_s3_client(config: &AppConfig) -> aws_sdk_s3::Client {
    build_client(&config.s3_region, config.s3_endpoint.as_deref()).await
}

/// Build the S3 client used for signing presigned upload URLs returned to
/// browsers. AWS SigV4 signs the `Host` header, so the signing endpoint must
/// be the one the browser can reach. Falls back to `s3_endpoint` when
/// `s3_public_endpoint` is not set (correct for real AWS and single-network
/// deployments).
pub async fn create_s3_presign_client(config: &AppConfig) -> aws_sdk_s3::Client {
    let endpoint = config
        .s3_public_endpoint
        .as_deref()
        .or(config.s3_endpoint.as_deref());
    build_client(&config.s3_region, endpoint).await
}

async fn build_client(region: &str, endpoint: Option<&str>) -> aws_sdk_s3::Client {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region.to_string()));
    let aws_config = if let Some(endpoint) = endpoint {
        aws_config.endpoint_url(endpoint).load().await
    } else {
        aws_config.load().await
    };

    // Path-style addressing is required by MinIO and most self-hosted S3
    // implementations. Real AWS keeps the default virtual-hosted style.
    let s3_config = aws_sdk_s3::config::Builder::from(&aws_config)
        .force_path_style(endpoint.is_some())
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

/// Open a Redis client. Connections are negotiated lazily per call inside
/// the publisher, consumer, and lock adapters.
pub fn create_redis_client(redis_url: &str) -> Result<redis::Client, redis::RedisError> {
    redis::Client::open(redis_url)
}
