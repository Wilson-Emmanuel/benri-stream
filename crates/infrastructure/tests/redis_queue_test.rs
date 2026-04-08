#![cfg(feature = "test-support")]

//! The publisher/consumer share a single hardcoded queue key, so these
//! tests serialize against a static async mutex to prevent parallel
//! `cargo test` workers from popping each other's messages.

use tokio::sync::Mutex;

use domain::ports::task::{TaskConsumer, TaskPublisher};
use domain::task::TaskId;
use infrastructure::redis::task_consumer::RedisTaskConsumer;
use infrastructure::redis::task_publisher::RedisTaskPublisher;
use infrastructure::testing::redis_client;

static QUEUE_LOCK: Mutex<()> = Mutex::const_new(());

async fn drain(consumer: &RedisTaskConsumer) {
    while consumer.pop().await.unwrap().is_some() {}
}

#[tokio::test]
async fn publish_then_pop_returns_ids_in_fifo_order() {
    let _guard = QUEUE_LOCK.lock().await;
    let publisher = RedisTaskPublisher::new(redis_client().await);
    let consumer = RedisTaskConsumer::new(redis_client().await);
    drain(&consumer).await;

    let ids: Vec<TaskId> = (0..3).map(|_| TaskId::new()).collect();
    publisher.publish(&ids).await.unwrap();

    // LPUSH + RPOP → FIFO: first published = first popped.
    let mut popped = vec![];
    for _ in 0..3 {
        popped.push(consumer.pop().await.unwrap().unwrap());
    }
    assert_eq!(popped, ids);
}

#[tokio::test]
async fn pop_returns_none_on_empty_queue() {
    let _guard = QUEUE_LOCK.lock().await;
    let consumer = RedisTaskConsumer::new(redis_client().await);
    drain(&consumer).await;
    assert!(consumer.pop().await.unwrap().is_none());
}

#[tokio::test]
async fn publish_empty_batch_is_ok() {
    let publisher = RedisTaskPublisher::new(redis_client().await);
    publisher.publish(&[]).await.unwrap();
}
