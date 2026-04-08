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
    build_client(&config.s3_region, config.s3_endpoint.as_deref()).await
}

/// Build the second S3 client used exclusively to sign presigned URLs
/// returned to browsers.
///
/// AWS Signature v4 signs the `Host` header, so the endpoint used at
/// signing time becomes the host the browser must actually reach. In
/// docker-compose, the internal endpoint (`http://minio:9000`) is not
/// browser-reachable, so presigning has to use a separate
/// browser-reachable endpoint like `http://localhost:9000`. Callers
/// pass [`AppConfig::s3_public_endpoint`] to pick it up; when unset,
/// presigning reuses the internal client and the caller is expected
/// to be in an environment where both sides share a network.
pub async fn create_s3_presign_client(config: &AppConfig) -> aws_sdk_s3::Client {
    // When no separate public endpoint is configured, fall back to the
    // internal endpoint — safe for real AWS S3 and for deployments
    // where the API and viewer share a network.
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

    // Force path-style addressing when a custom endpoint is set.
    // MinIO and most self-hosted S3 implementations do not support
    // the virtual-hosted `<bucket>.<host>` form AWS defaults to.
    // Real AWS (no custom endpoint) keeps the default virtual-hosted
    // behavior, which is what current AWS prefers.
    let s3_config = aws_sdk_s3::config::Builder::from(&aws_config)
        .force_path_style(endpoint.is_some())
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

/// Open a Redis client (multiplexed connections are negotiated lazily
/// per call inside the publisher / consumer / lock adapters).
pub fn create_redis_client(redis_url: &str) -> Result<redis::Client, redis::RedisError> {
    redis::Client::open(redis_url)
}
