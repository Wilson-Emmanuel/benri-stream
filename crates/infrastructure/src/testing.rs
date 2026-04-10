//! Integration-test fixtures shared across `infrastructure`, `api`, and
//! `worker` crates. Gated behind the `test-support` feature.
//!
//! One container per kind is started on first use and leaked for the
//! lifetime of the test binary. Each test builds its own client/pool
//! against the container's host+port to avoid cross-runtime handle issues.

use std::sync::OnceLock;

use aws_sdk_s3::config::{Credentials, Region};
use sqlx::PgPool;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;

pub const TEST_UPLOAD_BUCKET: &str = "benri-uploads";
pub const TEST_OUTPUT_BUCKET: &str = "benri-stream";

/// Endpoint details for the shared Postgres container. Tests create
/// their own `PgPool` against `url` so each test owns its runtime-bound
/// connections.
pub struct PgEndpoint {
    pub url: String,
}

/// Endpoint details for the shared MinIO container. Tests construct
/// their own `aws_sdk_s3::Client` via [`MinioEndpoint::client`].
pub struct MinioEndpoint {
    pub endpoint: String,
    pub upload_bucket: String,
    pub output_bucket: String,
}

/// Endpoint details for the shared Redis container.
pub struct RedisEndpoint {
    pub url: String,
}

// Leaked for the lifetime of the test binary. Handles carry no client
// state, so cross-runtime reuse is safe.
static PG_CONTAINER: OnceLock<ContainerAsync<Postgres>> = OnceLock::new();
static MINIO_CONTAINER: OnceLock<ContainerAsync<GenericImage>> = OnceLock::new();
static REDIS_CONTAINER: OnceLock<ContainerAsync<Redis>> = OnceLock::new();

static PG_ENDPOINT: OnceLock<PgEndpoint> = OnceLock::new();
static MINIO_ENDPOINT: OnceLock<MinioEndpoint> = OnceLock::new();
static REDIS_ENDPOINT: OnceLock<RedisEndpoint> = OnceLock::new();

// Async mutex so it can be held across container startup awaits without
// blocking the thread. Subsequent callers short-circuit on `OnceLock::get`.
static INIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start (or return) the Postgres fixture. Runs migrations once on first call.
pub async fn pg_endpoint() -> &'static PgEndpoint {
    if let Some(ep) = PG_ENDPOINT.get() {
        return ep;
    }

    // Serialise startup so concurrent tests don't race into two containers.
    let _guard = INIT_LOCK.lock().await;
    if let Some(ep) = PG_ENDPOINT.get() {
        return ep;
    }

    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("pg host");
    let port = container.get_host_port_ipv4(5432).await.expect("pg port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    // Run migrations on a throwaway pool, then close it before any
    // test-owned pools are created.
    let migration_pool = PgPool::connect(&url).await.expect("pg connect for migrate");
    let migration = include_str!("../../../migrations/001_init.sql");
    sqlx::raw_sql(migration)
        .execute(&migration_pool)
        .await
        .expect("run migration");
    migration_pool.close().await;

    let _ = PG_CONTAINER.set(container);
    let _ = PG_ENDPOINT.set(PgEndpoint { url });
    PG_ENDPOINT.get().unwrap()
}

/// Build a fresh `PgPool` bound to the current runtime.
pub async fn pg_pool() -> PgPool {
    let ep = pg_endpoint().await;
    PgPool::connect(&ep.url).await.expect("pg connect")
}

/// Start (or return) the MinIO fixture, creating both production buckets on
/// first call.
pub async fn minio_endpoint() -> &'static MinioEndpoint {
    if let Some(ep) = MINIO_ENDPOINT.get() {
        return ep;
    }

    let _guard = INIT_LOCK.lock().await;
    if let Some(ep) = MINIO_ENDPOINT.get() {
        return ep;
    }

    let container = GenericImage::new("minio/minio", "latest")
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::message_on_stderr("API:"))
        .with_cmd(["server", "/data"])
        .with_env_var("MINIO_ROOT_USER", "minioadmin")
        .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
        .start()
        .await
        .expect("start minio container");
    let host = container.get_host().await.expect("minio host");
    let port = container
        .get_host_port_ipv4(9000)
        .await
        .expect("minio port");
    let endpoint = format!("http://{host}:{port}");

    let client = build_s3_client(&endpoint);
    for bucket in [TEST_UPLOAD_BUCKET, TEST_OUTPUT_BUCKET] {
        let _ = client.create_bucket().bucket(bucket).send().await;
    }

    let _ = MINIO_CONTAINER.set(container);
    let _ = MINIO_ENDPOINT.set(MinioEndpoint {
        endpoint,
        upload_bucket: TEST_UPLOAD_BUCKET.into(),
        output_bucket: TEST_OUTPUT_BUCKET.into(),
    });
    MINIO_ENDPOINT.get().unwrap()
}

/// Build a fresh S3 client against the shared MinIO container.
pub async fn minio_client() -> aws_sdk_s3::Client {
    let ep = minio_endpoint().await;
    build_s3_client(&ep.endpoint)
}

fn build_s3_client(endpoint: &str) -> aws_sdk_s3::Client {
    let credentials = Credentials::new("minioadmin", "minioadmin", None, None, "test");
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(endpoint)
        .credentials_provider(credentials)
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

/// Start (or return) the Redis fixture.
pub async fn redis_endpoint() -> &'static RedisEndpoint {
    if let Some(ep) = REDIS_ENDPOINT.get() {
        return ep;
    }

    let _guard = INIT_LOCK.lock().await;
    if let Some(ep) = REDIS_ENDPOINT.get() {
        return ep;
    }

    let container = Redis::default()
        .start()
        .await
        .expect("start redis container");
    let host = container.get_host().await.expect("redis host");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    let url = format!("redis://{host}:{port}");

    let _ = REDIS_CONTAINER.set(container);
    let _ = REDIS_ENDPOINT.set(RedisEndpoint { url });
    REDIS_ENDPOINT.get().unwrap()
}

/// Build a fresh Redis client against the shared Redis container.
pub async fn redis_client() -> redis::Client {
    let ep = redis_endpoint().await;
    redis::Client::open(ep.url.clone()).expect("open redis client")
}
