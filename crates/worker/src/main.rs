mod consumer;
mod poller;
mod recovery;
mod system_checker;
mod handlers;

use std::collections::HashMap;
use std::sync::Arc;

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
use infrastructure::bootstrap::{create_pg_pool, create_redis_client, create_s3_client};
use infrastructure::config::AppConfig;
use infrastructure::postgres::transaction::PgTransactionPort;
use infrastructure::postgres::video_repository::PostgresVideoRepository;
use infrastructure::postgres::task_repository::PostgresTaskRepository;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::transcoder::gstreamer::GstreamerTranscoder;
use infrastructure::redis::task_publisher::RedisTaskPublisher;
use infrastructure::redis::task_consumer::RedisTaskConsumer;
use infrastructure::redis::distributed_lock::RedisDistributedLock;

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
    let pool = create_pg_pool(&config.database_url, 5)
        .await
        .expect("Failed to connect to database");
    tracing::info!("worker: database connected");

    let s3_client = create_s3_client(&config).await;
    let storage: Arc<dyn domain::ports::storage::StoragePort> = Arc::new(
        S3StorageClient::new(
            s3_client,
            config.s3_upload_bucket.clone(),
            config.s3_output_bucket.clone(),
            config.cdn_base_url.clone(),
        ),
    );

    tracing::info!("worker: connecting to redis");
    let redis_client = create_redis_client(&config.redis_url).expect("Invalid Redis URL");

    // Repositories + TransactionPort
    let video_repo: Arc<dyn domain::ports::video::VideoRepository> =
        Arc::new(PostgresVideoRepository::new(pool.clone()));
    let task_repo: Arc<dyn domain::ports::task::TaskRepository> =
        Arc::new(PostgresTaskRepository::new(pool.clone()));
    let tx_port: Arc<dyn domain::ports::transaction::TransactionPort> =
        Arc::new(PgTransactionPort::new(pool));

    // Transcoder
    let transcoder: Arc<dyn domain::ports::transcoder::TranscoderPort> =
        Arc::new(GstreamerTranscoder::new(storage.clone()));

    // Use cases
    let process_video_uc = Arc::new(ProcessVideoUseCase::new(
        video_repo.clone(), tx_port.clone(), storage.clone(), transcoder,
    ));
    let cleanup_uc = Arc::new(CleanupStaleVideosUseCase::new(
        video_repo.clone(), task_repo.clone(),
    ));
    let delete_video_uc = Arc::new(DeleteVideoUseCase::new(
        video_repo.clone(), storage.clone(),
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

    // Distributed lock — single shared instance behind the port.
    let lock: Arc<dyn domain::ports::distributed_lock::DistributedLockPort> =
        Arc::new(RedisDistributedLock::new(redis_client.clone()));

    // Worker components
    let task_consumer = consumer::TaskConsumerLoop::new(
        consumer_port, task_repo.clone(), handler,
    );
    let outbox_poller = poller::OutboxPoller::new(
        task_repo.clone(), publisher, lock.clone(),
    );
    let stale_recovery = recovery::StaleRecovery::new(
        task_repo.clone(), lock.clone(),
    );
    let system_checker = system_checker::SystemTaskChecker::new(
        task_repo, lock,
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
    // components to drain. A second signal during the drain forces an
    // immediate exit so an operator can kill a worker stuck inside a
    // long-running handler (e.g. transcoding) without having to find
    // the PID and SIGKILL it manually.
    wait_for_shutdown().await;
    tracing::info!("shutdown signal received, draining workers");
    let _ = shutdown_tx.send(true);
    spawn_force_exit_on_second_signal();

    let (consumer_result, poller_result, recovery_result, checker_result) =
        tokio::join!(consumer_handle, poller_handle, recovery_handle, checker_handle);

    // Surface any panics. tokio::spawn catches panics into the
    // JoinHandle so the runtime stays alive, but ignoring these errors
    // would let a panicked component die silently while the worker kept
    // running with one fewer component. Logging here makes the failure
    // visible at process exit time at minimum; a follow-up could add a
    // supervisor that restarts failed components at runtime.
    log_join_result("consumer", consumer_result);
    log_join_result("poller", poller_result);
    log_join_result("recovery", recovery_result);
    log_join_result("system_checker", checker_result);

    tracing::info!("Worker stopped");
}

fn log_join_result(component: &str, result: Result<(), tokio::task::JoinError>) {
    match result {
        Ok(()) => {}
        Err(e) if e.is_panic() => {
            let panic_msg = panic_message(e.into_panic());
            tracing::error!(component, panic = %panic_msg, "worker component panicked");
        }
        Err(e) => {
            tracing::warn!(component, error = %e, "worker component cancelled");
        }
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
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

/// After the first shutdown signal has triggered the graceful drain,
/// install a background watcher that forces an immediate exit on the
/// next signal. Standard exit code 130 (128 + SIGINT) so process
/// supervisors recognize it as "killed by operator."
fn spawn_force_exit_on_second_signal() {
    tokio::spawn(async move {
        wait_for_shutdown().await;
        tracing::error!("second shutdown signal received — forcing immediate exit");
        std::process::exit(130);
    });
}
