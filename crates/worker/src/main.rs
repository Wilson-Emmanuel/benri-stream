mod consumer;
mod poller;
mod recovery;
mod system_checker;
mod handlers;

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tokio::signal;
use tokio::sync::watch;

use application::usecases::video::{
    cleanup_stale_videos::CleanupStaleVideosUseCase,
    delete_video::DeleteVideoUseCase,
    process_video::ProcessVideoUseCase,
};
use domain::task::metadata::cleanup_stale_videos::CleanupStaleVideosTaskMetadata;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::task::metadata::process_video::ProcessVideoTaskMetadata;
use infrastructure::config::AppConfig;
use infrastructure::postgres::unit_of_work::PgUnitOfWork;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::transcoder::gstreamer::GstreamerTranscoder;
use infrastructure::redis::task_publisher::RedisTaskPublisher;
use infrastructure::redis::task_consumer::RedisTaskConsumer;
use infrastructure::redis::distributed_lock::DistributedLock;

use handlers::{ErasedHandler, HandlerAdapter, HandlerDispatch, TaskHandlerInvoker};
use handlers::cleanup_stale::CleanupStaleHandler;
use handlers::delete_video::DeleteVideoHandler;
use handlers::process_video::ProcessVideoHandler;

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

    tracing::info!("worker: connecting to database");
    // Database
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .expect("Failed to connect to database");
    tracing::info!("worker: database connected");

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
    tracing::info!("worker: connecting to redis");
    let redis_client =
        redis::Client::open(config.redis_url.as_str()).expect("Invalid Redis URL");

    // Repositories + UnitOfWork
    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn domain::ports::task::TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool.clone()));
    let uow: Arc<dyn domain::ports::unit_of_work::UnitOfWork> =
        Arc::new(PgUnitOfWork::new(pool));

    // Transcoder
    let transcoder: Arc<dyn domain::ports::transcoder::TranscoderPort> =
        Arc::new(GstreamerTranscoder::new(storage.clone()));

    // Use cases
    let process_video_uc = Arc::new(ProcessVideoUseCase::new(
        video_repo.clone(), uow.clone(), storage.clone(), transcoder,
    ));
    let cleanup_uc = Arc::new(CleanupStaleVideosUseCase::new(
        video_repo.clone(), uow.clone(),
    ));
    let delete_video_uc = Arc::new(DeleteVideoUseCase::new(
        video_repo.clone(), uow.clone(), storage.clone(),
    ));

    // Handler dispatch map — one entry per task type. The adapter
    // deserializes the task's metadata Value into the concrete type before
    // invoking the typed handler.
    let process_handler = Arc::new(ProcessVideoHandler::new(process_video_uc));
    let cleanup_handler = Arc::new(CleanupStaleHandler::new(cleanup_uc));
    let delete_handler = Arc::new(DeleteVideoHandler::new(delete_video_uc));

    let mut handler_map: HashMap<String, Arc<dyn ErasedHandler>> = HashMap::new();
    handler_map.insert(
        ProcessVideoTaskMetadata::METADATA_TYPE.to_string(),
        HandlerAdapter::wrap(process_handler),
    );
    handler_map.insert(
        CleanupStaleVideosTaskMetadata::METADATA_TYPE.to_string(),
        HandlerAdapter::wrap(cleanup_handler),
    );
    handler_map.insert(
        DeleteVideoTaskMetadata::METADATA_TYPE.to_string(),
        HandlerAdapter::wrap(delete_handler),
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
        task_repo, uow, DistributedLock::new(redis_client),
    );

    // Shutdown signal — all long-running components observe this and drain
    // gracefully when the process is asked to stop.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tracing::info!("Worker started");

    let consumer_shutdown = shutdown_rx.clone();
    let poller_shutdown = shutdown_rx.clone();
    let recovery_shutdown = shutdown_rx.clone();
    let checker_shutdown = shutdown_rx;

    let consumer_handle = tokio::spawn(async move {
        task_consumer.run(consumer_shutdown).await;
    });
    let poller_handle = tokio::spawn(async move {
        outbox_poller.run(poller_shutdown).await;
    });
    let recovery_handle = tokio::spawn(async move {
        stale_recovery.run(recovery_shutdown).await;
    });
    let checker_handle = tokio::spawn(async move {
        system_checker.run(checker_shutdown).await;
    });

    // Wait for SIGINT / SIGTERM, then broadcast shutdown and wait for all
    // components to drain.
    wait_for_shutdown().await;
    tracing::info!("shutdown signal received, draining workers");
    let _ = shutdown_tx.send(true);

    let _ = tokio::join!(consumer_handle, poller_handle, recovery_handle, checker_handle);
    tracing::info!("Worker stopped");
}

#[cfg(unix)]
async fn wait_for_shutdown() {
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    tokio::select! {
        _ = signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown() {
    let _ = signal::ctrl_c().await;
}
