mod handlers;

use axum::{routing::{get, post}, Router};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use application::usecases::video::{
    complete_upload::CompleteUploadUseCase,
    get_video_by_token::GetVideoByTokenUseCase,
    get_video_status::GetVideoStatusUseCase,
    initiate_upload::InitiateUploadUseCase,
};
use infrastructure::config::AppConfig;
use infrastructure::postgres::unit_of_work::PgUnitOfWork;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::storage::s3_client::S3StorageClient;

#[derive(Clone)]
pub struct AppState {
    pub initiate_upload: Arc<InitiateUploadUseCase>,
    pub complete_upload: Arc<CompleteUploadUseCase>,
    pub get_video_status: Arc<GetVideoStatusUseCase>,
    pub get_video_by_token: Arc<GetVideoByTokenUseCase>,
}

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

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("Failed to connect to database");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.s3_region.clone()));
    let aws_config = if let Some(endpoint) = &config.s3_endpoint {
        aws_config.endpoint_url(endpoint).load().await
    } else {
        aws_config.load().await
    };
    let s3_client = aws_sdk_s3::Client::new(&aws_config);

    let storage: Arc<dyn domain::ports::storage::StoragePort> = Arc::new(
        S3StorageClient::new(s3_client, config.s3_bucket.clone(), config.cdn_base_url.clone()),
    );

    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let uow: Arc<dyn domain::ports::unit_of_work::UnitOfWork> =
        Arc::new(PgUnitOfWork::new(pool));

    let state = AppState {
        initiate_upload: Arc::new(InitiateUploadUseCase::new(uow.clone(), storage.clone())),
        complete_upload: Arc::new(CompleteUploadUseCase::new(video_repo.clone(), uow.clone(), storage.clone())),
        get_video_status: Arc::new(GetVideoStatusUseCase::new(video_repo.clone(), config.base_url.clone())),
        get_video_by_token: Arc::new(GetVideoByTokenUseCase::new(video_repo.clone(), storage.clone())),
    };

    let app = Router::new()
        .route("/api/videos/initiate", post(handlers::video::initiate_upload))
        .route("/api/videos/{id}/complete", post(handlers::video::complete_upload))
        .route("/api/videos/{id}/status", get(handlers::video::get_video_status))
        .route("/api/videos/share/{share_token}", get(handlers::video::get_video_by_token))
        .route("/health", get(|| async { "ok" }))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr)
        .await
        .expect("Failed to bind");
    tracing::info!(addr = %config.listen_addr, "API server listening");
    axum::serve(listener, app).await.expect("Server failed");
}
