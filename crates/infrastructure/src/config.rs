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
    pub s3_endpoint: Option<String>,
    pub cdn_base_url: String,
    pub redis_url: String,
    pub listen_addr: String,
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
            cdn_base_url: env_or("CDN_BASE_URL", "http://localhost:8888"),
            redis_url: env_or("REDIS_URL", "redis://localhost:6379"),
            listen_addr: env_or("LISTEN_ADDR", "0.0.0.0:8080"),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
