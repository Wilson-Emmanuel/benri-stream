use std::sync::Arc;

use application::usecases::video::{
    complete_upload::CompleteUploadUseCase, get_video_by_token::GetVideoByTokenUseCase,
    get_video_status::GetVideoStatusUseCase, initiate_upload::InitiateUploadUseCase,
};
use infrastructure::bootstrap::{create_pg_pool, create_s3_client, create_s3_presign_client};
use infrastructure::config::AppConfig;
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::postgres::transaction::PgTransactionPort;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::storage::s3_client::S3StorageClient;

use api::{build_router, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".parse().unwrap()),
        )
        .json()
        .init();

    let config = AppConfig::from_env();

    tracing::info!("api: connecting to database");
    let pool = create_pg_pool(&config.database_url, 10)
        .await
        .expect("Failed to connect to database");

    tracing::info!("api: running migrations");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");
    tracing::info!("api: migrations applied");

    // The api signs upload URLs browsers will PUT to, so it needs
    // a second S3 client whose endpoint is browser-reachable. In
    // docker-compose this is `http://localhost:9000` via the host
    // port forward; the backend client uses `http://minio:9000` from
    // inside the container network. Against real AWS both endpoints
    // collapse to the same host and the override is a no-op.
    let s3_client = create_s3_client(&config).await;
    let s3_presign_client = create_s3_presign_client(&config).await;
    let storage: Arc<dyn domain::ports::storage::StoragePort> = Arc::new(
        S3StorageClient::new(
            s3_client,
            config.s3_upload_bucket.clone(),
            config.s3_output_bucket.clone(),
            config.cdn_base_url.clone(),
        )
        .with_upload_presign_client(s3_presign_client),
    );

    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn domain::ports::task::TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool.clone()));
    let tx_port: Arc<dyn domain::ports::transaction::TransactionPort> =
        Arc::new(PgTransactionPort::new(pool));

    let state = AppState {
        initiate_upload: Arc::new(InitiateUploadUseCase::new(video_repo.clone(), storage.clone())),
        complete_upload: Arc::new(CompleteUploadUseCase::new(
            video_repo.clone(),
            task_repo,
            tx_port,
            storage.clone(),
        )),
        get_video_status: Arc::new(GetVideoStatusUseCase::new(
            video_repo.clone(),
            config.base_url.clone(),
        )),
        get_video_by_token: Arc::new(GetVideoByTokenUseCase::new(
            video_repo.clone(),
            config.cdn_base_url.clone(),
        )),
    };

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr)
        .await
        .expect("Failed to bind");
    tracing::info!(addr = %config.listen_addr, "API server listening");
    axum::serve(listener, app).await.expect("Server failed");
}
