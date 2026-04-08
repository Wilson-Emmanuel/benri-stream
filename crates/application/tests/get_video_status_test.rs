use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::get_video_status::{Error, GetVideoStatusUseCase, Input};
use domain::ports::error::RepositoryError;
use domain::ports::video::MockVideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

fn video(id: VideoId, status: VideoStatus, share_token: Option<&str>) -> Video {
    Video {
        id,
        share_token: share_token.map(|s| s.into()),
        title: "t".into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: "uploads/x".into(),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn returns_status_and_share_url_when_processed() {
    let mut repo = MockVideoRepository::new();
    let id = VideoId::new();
    let id_cloned = id.clone();
    repo.expect_find_by_id().returning(move |_| {
        Ok(Some(video(id_cloned.clone(), VideoStatus::Processed, Some("tok"))))
    });

    let uc = GetVideoStatusUseCase::new(Arc::new(repo), "http://example.com".into());
    let out = uc.execute(Input { id }).await.unwrap();

    assert_eq!(out.status, VideoStatus::Processed);
    assert_eq!(out.share_url.as_deref(), Some("http://example.com/v/tok"));
}

#[tokio::test]
async fn returns_none_share_url_when_token_missing() {
    let mut repo = MockVideoRepository::new();
    let id = VideoId::new();
    let id_cloned = id.clone();
    repo.expect_find_by_id()
        .returning(move |_| Ok(Some(video(id_cloned.clone(), VideoStatus::Processing, None))));

    let uc = GetVideoStatusUseCase::new(Arc::new(repo), "http://example.com".into());
    let out = uc.execute(Input { id }).await.unwrap();

    assert_eq!(out.status, VideoStatus::Processing);
    assert!(out.share_url.is_none());
}

#[tokio::test]
async fn not_found_is_mapped_to_video_not_found() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_id().returning(|_| Ok(None));

    let uc = GetVideoStatusUseCase::new(Arc::new(repo), "http://example.com".into());
    let err = uc.execute(Input { id: VideoId::new() }).await.err();

    assert!(matches!(err, Some(Error::VideoNotFound)));
}

#[tokio::test]
async fn repo_error_is_mapped_to_internal() {
    let mut repo = MockVideoRepository::new();
    repo.expect_find_by_id()
        .returning(|_| Err(RepositoryError::Database("boom".into())));

    let uc = GetVideoStatusUseCase::new(Arc::new(repo), "http://example.com".into());
    let err = uc.execute(Input { id: VideoId::new() }).await.err();

    assert!(matches!(err, Some(Error::Internal(_))));
}
