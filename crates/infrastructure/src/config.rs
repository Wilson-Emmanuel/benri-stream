pub struct AppConfig {
    pub database_url: String,
    pub base_url: String,
    /// Private bucket holding original uploads under `uploads/{id}/...`.
    /// Worker reads via short-lived presigned GET URLs.
    pub s3_upload_bucket: String,
    /// Public-read bucket holding HLS output under `videos/{id}/...`.
    /// Fronted by the CDN; viewers fetch segments without auth.
    pub s3_output_bucket: String,
    pub s3_region: String,
    /// Endpoint the API and worker use internally to talk to S3/MinIO.
    /// Inside docker-compose this is the container-network hostname
    /// (e.g. `http://minio:9000`).
    pub s3_endpoint: Option<String>,
    /// Endpoint baked into presigned URLs returned to browsers. Must be
    /// a hostname the browser can reach. Defaults to `s3_endpoint` when
    /// unset — works for deployments where the API and the browser
    /// share a network (e.g. running against real AWS S3), but must be
    /// set explicitly for docker-compose, where the container-network
    /// hostname is not browser-reachable.
    pub s3_public_endpoint: Option<String>,
    pub cdn_base_url: String,
    pub redis_url: String,
    pub listen_addr: String,
    /// Comma-separated list of HLS quality tiers the worker transcodes
    /// for each video. Only the worker uses this — the api ignores it.
    /// Defaults to the full `low,medium,high` ladder; dev environments
    /// can override to `low` (or `low,medium`) to cut encode time
    /// proportionally when running on CPU-only hosts. Unknown entries
    /// are dropped and logged. See
    /// `infrastructure::transcoder::quality::parse_quality_tiers`.
    pub quality_tiers: String,
    /// Maximum number of tasks the worker's consumer loop can have
    /// in-flight at once. Only the worker uses this — the api ignores
    /// it. Defaults to `1` (one task at a time, matching the pre-
    /// semaphore behavior). Set higher on hosts with spare capacity
    /// so that e.g. a second `ProcessVideo` can start transcoding
    /// while the first one is still running. The ordering key on
    /// `ProcessVideoTaskMetadata` already prevents two concurrent
    /// attempts on the *same* video, so this knob is safe to raise
    /// without adding per-video races.
    pub worker_concurrency: usize,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: env_or("DATABASE_URL", "postgres://localhost:5432/benri_stream"),
            base_url: env_or("BASE_URL", "http://localhost:3000"),
            s3_upload_bucket: env_or("S3_UPLOAD_BUCKET", "benri-uploads"),
            s3_output_bucket: env_or("S3_OUTPUT_BUCKET", "benri-stream"),
            s3_region: env_or("S3_REGION", "us-east-1"),
            s3_endpoint: std::env::var("S3_ENDPOINT").ok(),
            s3_public_endpoint: std::env::var("S3_PUBLIC_ENDPOINT").ok(),
            cdn_base_url: env_or("CDN_BASE_URL", "http://localhost:8888"),
            redis_url: env_or("REDIS_URL", "redis://localhost:6379"),
            listen_addr: env_or("LISTEN_ADDR", "0.0.0.0:8080"),
            quality_tiers: env_or("QUALITY_TIERS", "low,medium,high"),
            worker_concurrency: env_or("WORKER_CONCURRENCY", "1")
                .parse()
                .unwrap_or_else(|_| {
                    tracing::warn!("WORKER_CONCURRENCY not a valid integer; defaulting to 1");
                    1
                }),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
