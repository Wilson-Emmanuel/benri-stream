mod common;

use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::complete_upload::{CompleteUploadUseCase, Error, Input};
use common::FakeTransactionPort;
use domain::ports::error::RepositoryError;
use domain::ports::storage::{MockStoragePort, ObjectMetadata, StorageError};
use domain::ports::task::MockTaskRepository;
use domain::ports::transaction::{MockTaskMutations, MockVideoMutations};
use domain::ports::video::MockVideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

fn mp4_video(id: VideoId, status: VideoStatus) -> Video {
    Video {
        id,
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: "uploads/x/original.mp4".into(),
        created_at: Utc::now(),
    }
}

fn ok_mp4_header() -> Vec<u8> {
    // bytes[4..8] == "ftyp" satisfies VideoFormat::Mp4.validate_signature
    let mut b = vec![0u8; 16];
    b[4..8].copy_from_slice(b"ftyp");
    b
}

#[tokio::test]
async fn happy_path_claims_and_schedules_process_video() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::PendingUpload))));

    let task_repo = MockTaskRepository::new();

    let mut storage = MockStoragePort::new();
    storage.expect_head_object().returning(|_| {
        Ok(Some(ObjectMetadata {
            size_bytes: 1024,
            content_type: Some("video/mp4".into()),
        }))
    });
    storage
        .expect_read_range()
        .returning(|_, _, _| Ok(ok_mp4_header()));

    // Transaction: claim succeeds, task scheduled.
    let mut video_muts = MockVideoMutations::new();
    video_muts
        .expect_update_status_if()
        .times(1)
        .returning(|_, _, _| Ok(true));
    let mut task_muts = MockTaskMutations::new();
    task_muts
        .expect_create()
        .times(1)
        .returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(video_muts, task_muts));

    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(task_repo),
        tx,
        Arc::new(storage),
    );

    let out = uc.execute(Input { id }).await.unwrap();
    assert_eq!(out.status, VideoStatus::Uploaded);
}

#[tokio::test]
async fn video_not_found_returns_error() {
    let mut video_repo = MockVideoRepository::new();
    video_repo.expect_find_by_id().returning(|_| Ok(None));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(MockTaskRepository::new()),
        tx,
        Arc::new(MockStoragePort::new()),
    );

    let err = uc.execute(Input { id: VideoId::new() }).await.err();
    assert!(matches!(err, Some(Error::VideoNotFound)));
}

#[tokio::test]
async fn non_pending_upload_returns_already_completed() {
    let id = VideoId::new();
    let id_c = id.clone();
    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::Uploaded))));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(MockTaskRepository::new()),
        tx,
        Arc::new(MockStoragePort::new()),
    );

    let err = uc.execute(Input { id }).await.err();
    assert!(matches!(err, Some(Error::AlreadyCompleted)));
}

#[tokio::test]
async fn file_missing_in_storage_returns_error() {
    let id = VideoId::new();
    let id_c = id.clone();
    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::PendingUpload))));

    let mut storage = MockStoragePort::new();
    storage
        .expect_head_object()
        .returning(|_| Err(StorageError::NotFound("k".into())));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(MockTaskRepository::new()),
        tx,
        Arc::new(storage),
    );

    let err = uc.execute(Input { id }).await.err();
    assert!(matches!(err, Some(Error::FileNotFoundInStorage)));
}

#[tokio::test]
async fn file_too_large_schedules_delete_and_returns_error() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::PendingUpload))));

    let mut storage = MockStoragePort::new();
    // 2 GB > 1 GB limit
    storage.expect_head_object().returning(|_| {
        Ok(Some(ObjectMetadata {
            size_bytes: 2 * 1024 * 1024 * 1024,
            content_type: Some("video/mp4".into()),
        }))
    });

    // Rejection path schedules DeleteVideo standalone.
    let mut task_repo = MockTaskRepository::new();
    task_repo
        .expect_create()
        .times(1)
        .returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(task_repo),
        tx,
        Arc::new(storage),
    );

    let err = uc.execute(Input { id }).await.err();
    assert!(matches!(err, Some(Error::FileTooLarge)));
}

#[tokio::test]
async fn invalid_signature_schedules_delete_and_returns_error() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::PendingUpload))));

    let mut storage = MockStoragePort::new();
    storage.expect_head_object().returning(|_| {
        Ok(Some(ObjectMetadata {
            size_bytes: 1024,
            content_type: Some("video/mp4".into()),
        }))
    });
    // All zeros — no `ftyp` at offset 4.
    storage.expect_read_range().returning(|_, _, _| Ok(vec![0u8; 16]));

    let mut task_repo = MockTaskRepository::new();
    task_repo
        .expect_create()
        .times(1)
        .returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(task_repo),
        tx,
        Arc::new(storage),
    );

    let err = uc.execute(Input { id }).await.err();
    assert!(matches!(err, Some(Error::InvalidFileSignature)));
}

#[tokio::test]
async fn lost_claim_race_returns_already_completed() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(mp4_video(id_c.clone(), VideoStatus::PendingUpload))));

    let mut storage = MockStoragePort::new();
    storage.expect_head_object().returning(|_| {
        Ok(Some(ObjectMetadata {
            size_bytes: 1024,
            content_type: Some("video/mp4".into()),
        }))
    });
    storage
        .expect_read_range()
        .returning(|_, _, _| Ok(ok_mp4_header()));

    let mut video_muts = MockVideoMutations::new();
    video_muts
        .expect_update_status_if()
        .returning(|_, _, _| Ok(false));
    let mut task_muts = MockTaskMutations::new();
    task_muts.expect_create().never();

    let tx = Arc::new(FakeTransactionPort::new(video_muts, task_muts));

    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(MockTaskRepository::new()),
        tx,
        Arc::new(storage),
    );

    let err = uc.execute(Input { id }).await.err();
    assert!(matches!(err, Some(Error::AlreadyCompleted)));
}

#[tokio::test]
async fn find_by_id_error_is_mapped_to_internal() {
    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(|_| Err(RepositoryError::Database("boom".into())));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = CompleteUploadUseCase::new(
        Arc::new(video_repo),
        Arc::new(MockTaskRepository::new()),
        tx,
        Arc::new(MockStoragePort::new()),
    );

    let err = uc.execute(Input { id: VideoId::new() }).await.err();
    assert!(matches!(err, Some(Error::Internal(_))));
}
