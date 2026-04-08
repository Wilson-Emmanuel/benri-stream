//! Integration-test fixtures shared across `infrastructure`, `api`, and
//! `worker` crates. Gated behind the `test-support` feature so it is
//! never compiled into release binaries.
//!
//! Each fixture launches a testcontainer on first use and leaks the
//! container handle for the lifetime of the test binary — containers
//! are torn down when the process exits. That keeps setup cost to one
//! cold start per test binary, not per test.

//! Test fixtures bring up one container per kind per test binary, then
//! let each `#[tokio::test]` build its own client/pool against the
//! container's host+port. Sharing the container (not the clients)
//! avoids the "tokio runtime shutdown" class of errors — each test
//! creates connections on its own runtime and drops them when done.
//!
//! Containers are leaked into a `OnceLock` so they stay alive until the
//! test binary exits. Bring-up is synchronized through a single
//! `std::sync::Mutex` + `OnceLock` pair so the first test blocks on
//! container start and every subsequent test sees a ready container.

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

// Containers are held in these statics so they are never dropped for
// the lifetime of the test binary. The handles carry no client state,
// so cross-runtime reuse is safe.
static PG_CONTAINER: OnceLock<ContainerAsync<Postgres>> = OnceLock::new();
static MINIO_CONTAINER: OnceLock<ContainerAsync<GenericImage>> = OnceLock::new();
static REDIS_CONTAINER: OnceLock<ContainerAsync<Redis>> = OnceLock::new();

static PG_ENDPOINT: OnceLock<PgEndpoint> = OnceLock::new();
static MINIO_ENDPOINT: OnceLock<MinioEndpoint> = OnceLock::new();
static REDIS_ENDPOINT: OnceLock<RedisEndpoint> = OnceLock::new();

// Async mutex: held across container startup awaits, so it must not
// block the whole thread. Only one first-init per endpoint ever
// acquires it — subsequent callers short-circuit on `OnceLock::get`.
static INIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Get (or start) the Postgres fixture. Runs migrations once; the
/// caller is responsible for creating its own pool against
/// [`PgEndpoint::url`].
pub async fn pg_endpoint() -> &'static PgEndpoint {
    if let Some(ep) = PG_ENDPOINT.get() {
        return ep;
    }

    // Guard the actual start so two concurrent `tokio::test`s on the
    // same binary don't race into two containers.
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

    // Run migrations on a throwaway pool. The real test pools are
    // created per-test, so this one is dropped as soon as the migration
    // finishes — no cross-runtime handles held anywhere.
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

/// Convenience: build a fresh `PgPool` bound to the current runtime.
pub async fn pg_pool() -> PgPool {
    let ep = pg_endpoint().await;
    PgPool::connect(&ep.url).await.expect("pg connect")
}

/// Get (or start) the MinIO fixture and create the two production
/// buckets. The caller builds its own S3 client via [`minio_client`].
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

    // One-shot bucket creation.
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

/// Build a fresh S3 client bound to the current runtime against the
/// shared MinIO container.
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

/// Get (or start) the Redis fixture.
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

/// Build a fresh Redis client against the shared container.
pub async fn redis_client() -> redis::Client {
    let ep = redis_endpoint().await;
    redis::Client::open(ep.url.clone()).expect("open redis client")
}
