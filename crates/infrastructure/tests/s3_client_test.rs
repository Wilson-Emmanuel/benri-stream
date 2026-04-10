#![cfg(feature = "test-support")]

use aws_sdk_s3::primitives::ByteStream;
use uuid::Uuid;

use domain::ports::storage::StoragePort;
use infrastructure::storage::s3_client::S3StorageClient;
use infrastructure::testing::{minio_client, minio_endpoint};

async fn fresh_client() -> S3StorageClient {
    let ep = minio_endpoint().await;
    let client = minio_client().await;
    S3StorageClient::new(
        client,
        ep.upload_bucket.clone(),
        ep.output_bucket.clone(),
        "http://cdn.test".into(),
    )
}

#[tokio::test]
async fn presigned_upload_url_is_generated() {
    let client = fresh_client().await;
    let key = format!("uploads/{}/original.mp4", Uuid::new_v4());
    let presigned = client
        .generate_presigned_upload_url(&key, "video/mp4", 1024, 60)
        .await
        .unwrap();
    assert!(presigned.url.starts_with("http://"));
    assert!(presigned.url.contains(&key));
}

#[tokio::test]
async fn head_object_returns_none_for_missing_key() {
    let client = fresh_client().await;
    let missing = format!("uploads/{}/ghost.mp4", Uuid::new_v4());
    assert!(client.head_object(&missing).await.unwrap().is_none());
}

#[tokio::test]
async fn put_then_head_then_read_range_then_delete_round_trip() {
    let ep = minio_endpoint().await;
    let raw = minio_client().await;
    let client = fresh_client().await;

    let key = format!("uploads/{}/data.bin", Uuid::new_v4());
    let body = b"hello-benri-stream-integration-test".to_vec();

    // Put via the raw SDK — upload_from_path requires a filesystem path.
    raw.put_object()
        .bucket(&ep.upload_bucket)
        .key(&key)
        .body(ByteStream::from(body.clone()))
        .content_type("application/octet-stream")
        .send()
        .await
        .unwrap();

    let meta = client.head_object(&key).await.unwrap().unwrap();
    assert_eq!(meta.size_bytes, body.len() as i64);

    let head = client
        .read_range(&key, 0, 4)
        .await
        .unwrap();
    assert_eq!(head, &body[..5]);

    client.delete_object(&key).await.unwrap();
    assert!(client.head_object(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_prefix_removes_all_keys_under_the_prefix() {
    let ep = minio_endpoint().await;
    let raw = minio_client().await;
    let client = fresh_client().await;

    // Use videos/ prefix so the adapter routes to the output bucket.
    let root = format!("videos/{}", Uuid::new_v4());
    for name in ["master.m3u8", "v0/seg0.ts", "v1/seg0.ts"] {
        raw.put_object()
            .bucket(&ep.output_bucket)
            .key(format!("{root}/{name}"))
            .body(ByteStream::from(b"x".to_vec()))
            .send()
            .await
            .unwrap();
    }

    client
        .delete_prefix(&format!("{root}/"))
        .await
        .unwrap();

    // Verify all three are gone.
    for name in ["master.m3u8", "v0/seg0.ts", "v1/seg0.ts"] {
        assert!(client
            .head_object(&format!("{root}/{name}"))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn delete_prefix_on_empty_prefix_is_a_noop() {
    let client = fresh_client().await;
    let root = format!("videos/{}/", Uuid::new_v4());
    client.delete_prefix(&root).await.unwrap();
}

#[tokio::test]
async fn routing_sends_uploads_and_videos_to_different_buckets() {
    // Put via the raw client into each bucket, then head via the adapter
    // which must route to the correct bucket by prefix.
    let ep = minio_endpoint().await;
    let raw = minio_client().await;
    let client = fresh_client().await;

    let upload_key = format!("uploads/{}/a.mp4", Uuid::new_v4());
    raw.put_object()
        .bucket(&ep.upload_bucket)
        .key(&upload_key)
        .body(ByteStream::from(b"a".to_vec()))
        .send()
        .await
        .unwrap();

    let video_key = format!("videos/{}/master.m3u8", Uuid::new_v4());
    raw.put_object()
        .bucket(&ep.output_bucket)
        .key(&video_key)
        .body(ByteStream::from(b"v".to_vec()))
        .send()
        .await
        .unwrap();

    assert!(client.head_object(&upload_key).await.unwrap().is_some());
    assert!(client.head_object(&video_key).await.unwrap().is_some());
}

#[tokio::test]
async fn public_url_formats_with_cdn_base() {
    let client = fresh_client().await;
    let url = client.public_url("videos/abc/master.m3u8");
    assert_eq!(url, "http://cdn.test/videos/abc/master.m3u8");
}
