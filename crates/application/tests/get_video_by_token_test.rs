use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::get_video_by_token::{Error, GetVideoByTokenUseCase, Input};
use domain::ports::error::RepositoryError;
use domain::ports::video::MockVideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

fn video(status: VideoStatus, title: &str) -> Video {
    Video {
        id: VideoId::new(),
        share_token: Some("tok".into()),
        title: title.into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: "uploads/x".into(),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn returns_title_and_stream_url_for_processed_video() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_share_token()
        .returning(|_| Ok(Some(video(VideoStatus::Processed, "Clip"))));

    let uc = GetVideoByTokenUseCase::new(Arc::new(repo), "http://cdn.example".into());
    let out = uc
        .execute(Input {
            share_token: "tok".into(),
        })
        .await
        .unwrap();

    assert_eq!(out.title, "Clip");
    let url = out.stream_url.expect("expected stream url");
    assert!(url.starts_with("http://cdn.example/videos/"));
    assert!(url.ends_with("/master.m3u8"));
}

#[tokio::test]
async fn trailing_slash_in_cdn_is_normalised() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_share_token()
        .returning(|_| Ok(Some(video(VideoStatus::Processed, "x"))));

    let uc = GetVideoByTokenUseCase::new(Arc::new(repo), "http://cdn.example/".into());
    let out = uc
        .execute(Input {
            share_token: "tok".into(),
        })
        .await
        .unwrap();

    let url = out.stream_url.unwrap();
    assert!(!url.contains("//videos"));
    assert!(url.starts_with("http://cdn.example/videos/"));
}

#[tokio::test]
async fn returns_none_stream_url_when_not_processed() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_share_token()
        .returning(|_| Ok(Some(video(VideoStatus::Processing, "x"))));

    let uc = GetVideoByTokenUseCase::new(Arc::new(repo), "http://cdn.example".into());
    let out = uc
        .execute(Input {
            share_token: "tok".into(),
        })
        .await
        .unwrap();

    assert!(out.stream_url.is_none());
}

#[tokio::test]
async fn not_found_is_mapped_to_video_not_found() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_share_token().returning(|_| Ok(None));

    let uc = GetVideoByTokenUseCase::new(Arc::new(repo), "http://cdn.example".into());
    let err = uc
        .execute(Input {
            share_token: "missing".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::VideoNotFound)));
}

#[tokio::test]
async fn repo_error_is_mapped_to_internal() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_share_token()
        .returning(|_| Err(RepositoryError::Database("boom".into())));

    let uc = GetVideoByTokenUseCase::new(Arc::new(repo), "http://cdn.example".into());
    let err = uc
        .execute(Input {
            share_token: "tok".into(),
        })
        .await
        .err();

    assert!(matches!(err, Some(Error::Internal(_))));
}
