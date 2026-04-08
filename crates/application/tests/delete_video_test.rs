use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::delete_video::{DeleteVideoUseCase, Error, Input};
use domain::ports::error::RepositoryError;
use domain::ports::storage::{MockStoragePort, StorageError};
use domain::ports::video::MockVideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

fn video(id: VideoId) -> Video {
    Video {
        id,
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::Failed,
        upload_key: "uploads/abc/original.mp4".into(),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn happy_path_deletes_prefix_original_and_row_in_order() {
    let id = VideoId::new();
    let id_cloned = id.clone();

    let mut repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();

    repo.expect_find_by_id()
        .returning(move |_| Ok(Some(video(id_cloned.clone()))));
    storage
        .expect_delete_prefix()
        .withf(|p| p.starts_with("videos/") && p.ends_with('/'))
        .times(1)
        .returning(|_| Ok(()));
    storage
        .expect_delete_object()
        .withf(|k| k == "uploads/abc/original.mp4")
        .times(1)
        .returning(|_| Ok(()));
    repo.expect_delete().times(1).returning(|_| Ok(()));

    let uc = DeleteVideoUseCase::new(Arc::new(repo), Arc::new(storage));
    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn missing_video_returns_video_not_found() {
    let mut repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    repo.expect_find_by_id().returning(|_| Ok(None));

    let uc = DeleteVideoUseCase::new(Arc::new(repo), Arc::new(storage));
    let err = uc
        .execute(Input {
            video_id: VideoId::new(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::VideoNotFound)));
}

#[tokio::test]
async fn prefix_delete_failure_halts_before_row_delete() {
    let id = VideoId::new();
    let id_cloned = id.clone();

    let mut repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();

    repo.expect_find_by_id()
        .returning(move |_| Ok(Some(video(id_cloned.clone()))));
    storage
        .expect_delete_prefix()
        .returning(|_| Err(StorageError::Internal("s3 down".into())));
    // delete_object / repo.delete should never be called — no expect set.
    repo.expect_delete().never();

    let uc = DeleteVideoUseCase::new(Arc::new(repo), Arc::new(storage));
    let err = uc.execute(Input { video_id: id }).await.err();
    assert!(matches!(err, Some(Error::Internal(_))));
}

#[tokio::test]
async fn original_delete_failure_halts_before_row_delete() {
    let id = VideoId::new();
    let id_cloned = id.clone();

    let mut repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();

    repo.expect_find_by_id()
        .returning(move |_| Ok(Some(video(id_cloned.clone()))));
    storage.expect_delete_prefix().returning(|_| Ok(()));
    storage
        .expect_delete_object()
        .returning(|_| Err(StorageError::Internal("nope".into())));
    repo.expect_delete().never();

    let uc = DeleteVideoUseCase::new(Arc::new(repo), Arc::new(storage));
    let err = uc.execute(Input { video_id: id }).await.err();
    assert!(matches!(err, Some(Error::Internal(_))));
}

#[tokio::test]
async fn find_error_is_mapped_to_internal() {
    let mut repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    repo.expect_find_by_id()
        .returning(|_| Err(RepositoryError::Database("boom".into())));

    let uc = DeleteVideoUseCase::new(Arc::new(repo), Arc::new(storage));
    let err = uc
        .execute(Input {
            video_id: VideoId::new(),
        })
        .await
        .err();
    assert!(matches!(err, Some(Error::Internal(_))));
}
