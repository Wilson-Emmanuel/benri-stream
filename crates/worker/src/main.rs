mod consumer;
mod poller;
mod recovery;
mod system_checker;
mod handlers;

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use application::usecases::video::{
    cleanup_stale_videos::CleanupStaleVideosUseCase,
    process_video::ProcessVideoUseCase,
};
use infrastructure::config::AppConfig;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::transcoder::gstreamer::GstreamerTranscoder;
use infrastructure::redis::task_publisher::RedisTaskPublisher;
use infrastructure::redis::task_consumer::RedisTaskConsumer;
use infrastructure::redis::distributed_lock::DistributedLock;

use handlers::{HandlerDispatch, TaskHandler, TaskHandlerInvoker};
use handlers::process_video::ProcessVideoHandler;
use handlers::cleanup_stale::CleanupStaleHandler;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .json()
        .init();

    let config = AppConfig::from_env();

    // Database
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .expect("Failed to connect to database");

    // S3
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

    // Redis
    let redis_client =
        redis::Client::open(config.redis_url.as_str()).expect("Invalid Redis URL");

    // Repositories
    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn domain::ports::task::TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool));

    // Transcoder
    let transcoder: Arc<dyn domain::ports::transcoder::TranscoderPort> =
        Arc::new(GstreamerTranscoder::new(storage.clone()));

    // Use cases
    let process_video = Arc::new(ProcessVideoUseCase::new(
        video_repo.clone(), storage.clone(), transcoder,
    ));
    let cleanup = Arc::new(CleanupStaleVideosUseCase::new(
        video_repo.clone(), storage.clone(),
    ));

    // Handler dispatch map
    let mut handler_map: HashMap<String, Arc<dyn TaskHandler>> = HashMap::new();
    handler_map.insert(
        "ProcessVideoTaskMetadata".to_string(),
        Arc::new(ProcessVideoHandler::new(process_video)),
    );
    handler_map.insert(
        "CleanupStaleVideosTaskMetadata".to_string(),
        Arc::new(CleanupStaleHandler::new(cleanup)),
    );
    let handler: Arc<dyn TaskHandlerInvoker> = Arc::new(HandlerDispatch::new(handler_map));

    // Queue
    let publisher: Arc<dyn domain::ports::task::TaskPublisher> =
        Arc::new(RedisTaskPublisher::new(redis_client.clone()));
    let consumer_port: Arc<dyn domain::ports::task::TaskConsumer> =
        Arc::new(RedisTaskConsumer::new(redis_client.clone()));

    // Worker components
    let task_consumer = consumer::TaskConsumerLoop::new(
        consumer_port, task_repo.clone(), handler,
    );
    let outbox_poller = poller::OutboxPoller::new(
        task_repo.clone(), publisher, DistributedLock::new(redis_client.clone()),
    );
    let stale_recovery = recovery::StaleRecovery::new(
        task_repo.clone(), DistributedLock::new(redis_client.clone()),
    );
    let system_checker = system_checker::SystemTaskChecker::new(
        task_repo, DistributedLock::new(redis_client),
    );

    tracing::info!("Worker started");

    tokio::select! {
        _ = task_consumer.run() => {},
        _ = outbox_poller.run() => {},
        _ = stale_recovery.run() => {},
        _ = system_checker.run() => {},
    }
}
