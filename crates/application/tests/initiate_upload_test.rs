use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::initiate_upload::{Error, InitiateUploadUseCase, Input};
use domain::ports::storage::{MockStoragePort, PresignedUpload, StorageError};
use domain::ports::video::MockVideoRepository;

fn presigned() -> PresignedUpload {
    PresignedUpload {
        url: "https://s3.example/upload-url".into(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
    }
}

#[tokio::test]
async fn happy_path_inserts_video_and_returns_upload_url() {
    let mut video_repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();

    video_repo.expect_insert().times(1).returning(|_| Ok(()));
    storage
        .expect_generate_presigned_upload_url()
        .times(1)
        .returning(|_, _, _, _| Ok(presigned()));

    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));
    let out = uc
        .execute(Input {
            title: "My clip".into(),
            mime_type: "video/mp4".into(),
        })
        .await
        .unwrap();

    assert_eq!(out.upload_url, "https://s3.example/upload-url");
}

#[tokio::test]
async fn empty_title_returns_title_required() {
    let video_repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));

    let err = uc
        .execute(Input {
            title: "   ".into(),
            mime_type: "video/mp4".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::TitleRequired)));
}

#[tokio::test]
async fn title_over_100_chars_is_rejected() {
    let video_repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));

    let err = uc
        .execute(Input {
            title: "a".repeat(101),
            mime_type: "video/mp4".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::TitleTooLong)));
}

#[tokio::test]
async fn title_with_100_emojis_is_accepted() {
    // 100 emoji chars = ~400 bytes. String::len would reject, chars().count() accepts.
    let mut video_repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();
    video_repo.expect_insert().returning(|_| Ok(()));
    storage
        .expect_generate_presigned_upload_url()
        .returning(|_, _, _, _| Ok(presigned()));

    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));
    let out = uc
        .execute(Input {
            title: "🎬".repeat(100),
            mime_type: "video/mp4".into(),
        })
        .await;

    assert!(out.is_ok());
}

#[tokio::test]
async fn unsupported_mime_type_is_rejected() {
    let video_repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));

    let err = uc
        .execute(Input {
            title: "hi".into(),
            mime_type: "image/png".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::UnsupportedFormat)));
}

#[tokio::test]
async fn repo_insert_error_is_mapped_to_internal() {
    let mut video_repo = MockVideoRepository::new();
    let storage = MockStoragePort::new();
    video_repo.expect_insert().returning(|_| {
        Err(domain::ports::error::RepositoryError::Database("boom".into()))
    });

    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));
    let err = uc
        .execute(Input {
            title: "hi".into(),
            mime_type: "video/mp4".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::Internal(_))));
}

#[tokio::test]
async fn presign_error_is_mapped_to_internal() {
    let mut video_repo = MockVideoRepository::new();
    let mut storage = MockStoragePort::new();
    video_repo.expect_insert().returning(|_| Ok(()));
    storage
        .expect_generate_presigned_upload_url()
        .returning(|_, _, _, _| Err(StorageError::Internal("no".into())));

    let uc = InitiateUploadUseCase::new(Arc::new(video_repo), Arc::new(storage));
    let err = uc
        .execute(Input {
            title: "hi".into(),
            mime_type: "video/mp4".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::Internal(_))));
}
