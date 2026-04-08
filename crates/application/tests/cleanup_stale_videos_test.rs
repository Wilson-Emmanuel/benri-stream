use std::sync::{Arc, Mutex};

use chrono::Utc;

use application::usecases::video::cleanup_stale_videos::{CleanupStaleVideosUseCase, Error};
use domain::ports::error::RepositoryError;
use domain::ports::task::MockTaskRepository;
use domain::ports::video::MockVideoRepository;
use domain::task::metadata::delete_video::DeleteVideoTaskMetadata;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

fn video(status: VideoStatus) -> Video {
    Video {
        id: VideoId::new(),
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: "uploads/x".into(),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn empty_state_does_not_touch_task_repo() {
    let mut video_repo = MockVideoRepository::new();
    let mut task_repo = MockTaskRepository::new();

    video_repo.expect_find_stale().returning(|_| Ok(vec![]));
    video_repo.expect_find_failed_before().returning(|_| Ok(vec![]));
    video_repo.expect_bulk_mark_failed().never();
    task_repo.expect_bulk_create().never();

    let uc = CleanupStaleVideosUseCase::new(Arc::new(video_repo), Arc::new(task_repo));
    let stats = uc.execute().await.unwrap();
    assert_eq!(stats.pending_scheduled, 0);
    assert_eq!(stats.stuck_scheduled, 0);
    assert_eq!(stats.failed_scheduled, 0);
}

#[tokio::test]
async fn stuck_videos_are_marked_failed_and_scheduled() {
    // 1 PendingUpload + 1 Uploaded + 1 Processing stale, 2 old Failed.
    // Stuck count = 2 (Uploaded, Processing).
    // Total DeleteVideo tasks scheduled = 3 stale + 2 failed = 5.
    let mut video_repo = MockVideoRepository::new();
    let mut task_repo = MockTaskRepository::new();

    let stale = vec![
        video(VideoStatus::PendingUpload),
        video(VideoStatus::Uploaded),
        video(VideoStatus::Processing),
    ];
    let failed = vec![video(VideoStatus::Failed), video(VideoStatus::Failed)];

    video_repo
        .expect_find_stale()
        .returning(move |_| Ok(stale.clone()));
    video_repo
        .expect_find_failed_before()
        .returning(move |_| Ok(failed.clone()));

    let captured_ids: Arc<Mutex<Vec<VideoId>>> = Arc::new(Mutex::new(vec![]));
    let captured_statuses: Arc<Mutex<Vec<VideoStatus>>> = Arc::new(Mutex::new(vec![]));
    let cap_ids = captured_ids.clone();
    let cap_statuses = captured_statuses.clone();
    video_repo
        .expect_bulk_mark_failed()
        .times(1)
        .returning(move |ids, from| {
            *cap_ids.lock().unwrap() = ids.to_vec();
            *cap_statuses.lock().unwrap() = from.to_vec();
            Ok(())
        });

    let captured_tasks = Arc::new(Mutex::new(0usize));
    let captured_type = Arc::new(Mutex::new(String::new()));
    let cap_t = captured_tasks.clone();
    let cap_type = captured_type.clone();
    task_repo
        .expect_bulk_create()
        .times(1)
        .returning(move |tasks| {
            *cap_t.lock().unwrap() = tasks.len();
            if let Some(t) = tasks.first() {
                *cap_type.lock().unwrap() = t.metadata_type.clone();
            }
            Ok(())
        });

    let uc = CleanupStaleVideosUseCase::new(Arc::new(video_repo), Arc::new(task_repo));
    let stats = uc.execute().await.unwrap();

    assert_eq!(stats.pending_scheduled, 1);
    assert_eq!(stats.stuck_scheduled, 2);
    assert_eq!(stats.failed_scheduled, 2);

    assert_eq!(captured_ids.lock().unwrap().len(), 2);
    let statuses = captured_statuses.lock().unwrap().clone();
    assert!(statuses.contains(&VideoStatus::Uploaded));
    assert!(statuses.contains(&VideoStatus::Processing));

    assert_eq!(*captured_tasks.lock().unwrap(), 5);
    // Task metadata_type must match the DeleteVideo meta.
    let expected = DeleteVideoTaskMetadata {
        video_id: VideoId::new(),
    };
    assert_eq!(
        *captured_type.lock().unwrap(),
        <DeleteVideoTaskMetadata as domain::task::TaskMetadata>::metadata_type_name(&expected)
    );
}

#[tokio::test]
async fn find_stale_error_is_mapped_to_internal() {
    let mut video_repo = MockVideoRepository::new();
    let task_repo = MockTaskRepository::new();
    video_repo
        .expect_find_stale()
        .returning(|_| Err(RepositoryError::Database("boom".into())));

    let uc = CleanupStaleVideosUseCase::new(Arc::new(video_repo), Arc::new(task_repo));
    let err = uc.execute().await.err();
    assert!(matches!(err, Some(Error::Internal(_))));
}
