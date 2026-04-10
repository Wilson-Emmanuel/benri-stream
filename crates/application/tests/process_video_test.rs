mod common;

use std::sync::Arc;

use chrono::Utc;

use application::usecases::video::process_video::{Error, Input, ProcessVideoUseCase};
use common::FakeTransactionPort;
use domain::ports::error::RepositoryError;
use domain::ports::storage::{MockStoragePort, StorageError};
use domain::ports::transaction::{MockTaskMutations, MockVideoMutations};
use domain::ports::transcoder::{
    FirstSegmentNotifier, MockTranscoderPort, ProbeResult, TranscoderError,
};
use domain::ports::video::MockVideoRepository;
use domain::video::{Video, VideoFormat, VideoId, VideoStatus};

/// Fires the notifier to simulate "first segment ready".
fn fire(notifier: Box<dyn FirstSegmentNotifier>) {
    notifier.notify();
}

fn uploaded_video(id: VideoId) -> Video {
    Video {
        id,
        share_token: None,
        title: "t".into(),
        format: VideoFormat::Mp4,
        status: VideoStatus::Uploaded,
        upload_key: "uploads/x/original.mp4".into(),
        created_at: Utc::now(),
    }
}

fn probe() -> ProbeResult {
    ProbeResult {
        duration_seconds: 10.0,
        width: 1280,
        height: 720,
        codec: "h264".into(),
        has_audio: true,
    }
}

#[tokio::test]
async fn happy_path_publishes_early_then_marks_processed_and_cleans_original() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .withf(|_, exp, new| *exp == VideoStatus::Uploaded && *new == VideoStatus::Processing)
        .times(1)
        .returning(|_, _, _| Ok(true));
    video_repo
        .expect_set_share_token()
        .times(1)
        .returning(|_, _| Ok(true));
    video_repo
        .expect_mark_processed()
        .times(1)
        .returning(|_, _| Ok(true));

    let mut storage = MockStoragePort::new();
    storage
        .expect_delete_object()
        .times(1)
        .returning(|_| Ok(()));

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, notifier| {
            fire(notifier);
            Ok(())
        });

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));

    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(storage),
        Arc::new(transcoder),
    );
    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn transcode_success_without_firing_notifier_still_marks_processed() {
    // Pathological case: transcoder returns Ok without firing the notifier.
    // mark_processed still writes the share token; set_share_token is never called.
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    video_repo.expect_set_share_token().never();
    video_repo
        .expect_mark_processed()
        .times(1)
        .returning(|_, _| Ok(true));

    let mut storage = MockStoragePort::new();
    storage.expect_delete_object().returning(|_| Ok(()));

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, _notifier| Ok(()));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(storage),
        Arc::new(transcoder),
    );
    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn video_not_found_returns_error() {
    let mut video_repo = MockVideoRepository::new();
    video_repo.expect_find_by_id().returning(|_| Ok(None));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(MockStoragePort::new()),
        Arc::new(MockTranscoderPort::new()),
    );

    let err = uc
        .execute(Input {
            video_id: VideoId::new(),
        })
        .await
        .err();
    assert!(matches!(err, Some(Error::VideoNotFound)));
}

#[tokio::test]
async fn lost_claim_is_noop_ok() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(false));
    // No probe / transcode / mark_processed should happen.
    video_repo.expect_mark_processed().never();

    let mut transcoder = MockTranscoderPort::new();
    // No probe or transcode should run.
    transcoder.expect_probe().never();

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(MockStoragePort::new()),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn probe_failure_transitions_to_failed_and_schedules_delete() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    video_repo.expect_mark_processed().never();

    let mut transcoder = MockTranscoderPort::new();
    transcoder
        .expect_probe()
        .returning(|_| Err(TranscoderError::ProbeFailed("bad codec".into())));
    transcoder.expect_transcode_to_hls().never();

    let mut video_muts = MockVideoMutations::new();
    video_muts
        .expect_update_status_if()
        .withf(|_, exp, new| *exp == VideoStatus::Processing && *new == VideoStatus::Failed)
        .times(1)
        .returning(|_, _, _| Ok(true));
    let mut task_muts = MockTaskMutations::new();
    task_muts
        .expect_create()
        .times(1)
        .returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(video_muts, task_muts));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(MockStoragePort::new()),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn transcode_failure_transitions_to_failed_and_schedules_delete() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    video_repo.expect_mark_processed().never();

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    // Notifier is dropped without firing — simulates transcode
    // failing before the first segment ever lands.
    // Notifier is dropped without firing — simulates failure before first segment.
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, _notifier| {
            Err(TranscoderError::TranscodeFailed("pipeline".into()))
        });

    let mut video_muts = MockVideoMutations::new();
    video_muts
        .expect_update_status_if()
        .times(1)
        .returning(|_, _, _| Ok(true));
    let mut task_muts = MockTaskMutations::new();
    task_muts.expect_create().times(1).returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(video_muts, task_muts));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(MockStoragePort::new()),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn mark_processed_no_row_is_noop() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    // Both return false — row moved out of Processing before each write.
    video_repo
        .expect_set_share_token()
        .returning(|_, _| Ok(false));
    video_repo
        .expect_mark_processed()
        .returning(|_, _| Ok(false));

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, notifier| {
            fire(notifier);
            Ok(())
        });

    let mut storage = MockStoragePort::new();
    storage.expect_delete_object().never();

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(storage),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn mark_processed_error_fails_the_video() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    video_repo
        .expect_set_share_token()
        .returning(|_, _| Ok(true));
    video_repo
        .expect_mark_processed()
        .returning(|_, _| Err(RepositoryError::Database("tx lost".into())));

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, notifier| {
            fire(notifier);
            Ok(())
        });

    let mut video_muts = MockVideoMutations::new();
    video_muts
        .expect_update_status_if()
        .times(1)
        .returning(|_, _, _| Ok(true));
    let mut task_muts = MockTaskMutations::new();
    task_muts.expect_create().times(1).returning(|t| Ok(t.clone()));

    let tx = Arc::new(FakeTransactionPort::new(video_muts, task_muts));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(MockStoragePort::new()),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}

#[tokio::test]
async fn cleanup_failure_after_success_is_tolerated() {
    let id = VideoId::new();
    let id_c = id.clone();

    let mut video_repo = MockVideoRepository::new();
    video_repo
        .expect_find_by_id()
        .returning(move |_| Ok(Some(uploaded_video(id_c.clone()))));
    video_repo
        .expect_update_status_if()
        .returning(|_, _, _| Ok(true));
    video_repo
        .expect_set_share_token()
        .returning(|_, _| Ok(true));
    video_repo
        .expect_mark_processed()
        .returning(|_, _| Ok(true));

    let mut transcoder = MockTranscoderPort::new();
    transcoder.expect_probe().returning(|_| Ok(probe()));
    transcoder
        .expect_transcode_to_hls()
        .returning(|_, _, _, notifier| {
            fire(notifier);
            Ok(())
        });

    let mut storage = MockStoragePort::new();
    storage
        .expect_delete_object()
        .returning(|_| Err(StorageError::Internal("orphan".into())));

    let tx = Arc::new(FakeTransactionPort::new(
        MockVideoMutations::new(),
        MockTaskMutations::new(),
    ));
    let uc = ProcessVideoUseCase::new(
        Arc::new(video_repo),
        tx,
        Arc::new(storage),
        Arc::new(transcoder),
    );

    uc.execute(Input { video_id: id }).await.unwrap();
}
