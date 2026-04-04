use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use application::usecases::video::{
    complete_upload, get_video_by_token, get_video_status, initiate_upload,
};
use domain::video::VideoId;

use crate::AppState;

#[derive(Deserialize)]
pub struct InitiateUploadRequest {
    pub title: String,
    pub mime_type: String,
}

#[derive(Serialize)]
pub struct InitiateUploadResponse {
    pub id: Uuid,
    pub upload_url: String,
}

#[derive(Serialize)]
pub struct CompleteUploadResponse {
    pub id: Uuid,
    pub status: String,
}

#[derive(Serialize)]
pub struct VideoStatusResponse {
    pub status: String,
    pub share_url: Option<String>,
}

#[derive(Serialize)]
pub struct VideoByTokenResponse {
    pub title: String,
    pub stream_url: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
}

pub async fn initiate_upload(
    State(state): State<AppState>,
    Json(body): Json<InitiateUploadRequest>,
) -> Result<Json<InitiateUploadResponse>, (StatusCode, Json<ErrorResponse>)> {
    let input = initiate_upload::Input {
        title: body.title,
        mime_type: body.mime_type,
    };

    state.initiate_upload.execute(input).await.map(|out| {
        Json(InitiateUploadResponse {
            id: out.id.0,
            upload_url: out.upload_url,
        })
    }).map_err(|e| {
        let (status, code) = match &e {
            initiate_upload::Error::TitleRequired => (StatusCode::BAD_REQUEST, "TITLE_REQUIRED"),
            initiate_upload::Error::TitleTooLong => (StatusCode::BAD_REQUEST, "TITLE_TOO_LONG"),
            initiate_upload::Error::UnsupportedFormat => (StatusCode::BAD_REQUEST, "UNSUPPORTED_FORMAT"),
            initiate_upload::Error::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        };
        (status, Json(ErrorResponse { error: e.to_string(), code: code.to_string() }))
    })
}

pub async fn complete_upload(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CompleteUploadResponse>, (StatusCode, Json<ErrorResponse>)> {
    let input = complete_upload::Input { id: VideoId(id) };

    state.complete_upload.execute(input).await.map(|out| {
        Json(CompleteUploadResponse {
            id: out.id.0,
            status: out.status.as_str().to_string(),
        })
    }).map_err(|e| {
        let (status, code) = match &e {
            complete_upload::Error::VideoNotFound => (StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND"),
            complete_upload::Error::AlreadyCompleted => (StatusCode::CONFLICT, "ALREADY_COMPLETED"),
            complete_upload::Error::FileNotFoundInStorage => (StatusCode::BAD_REQUEST, "FILE_NOT_FOUND_IN_STORAGE"),
            complete_upload::Error::FileTooLarge => (StatusCode::BAD_REQUEST, "FILE_TOO_LARGE"),
            complete_upload::Error::InvalidFileSignature => (StatusCode::BAD_REQUEST, "INVALID_FILE_SIGNATURE"),
            complete_upload::Error::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        };
        (status, Json(ErrorResponse { error: e.to_string(), code: code.to_string() }))
    })
}

pub async fn get_video_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<VideoStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let input = get_video_status::Input { id: VideoId(id) };

    state.get_video_status.execute(input).await.map(|out| {
        Json(VideoStatusResponse {
            status: out.status.as_str().to_string(),
            share_url: out.share_url,
        })
    }).map_err(|e| {
        let (status, code) = match &e {
            get_video_status::Error::VideoNotFound => (StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND"),
            get_video_status::Error::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        };
        (status, Json(ErrorResponse { error: e.to_string(), code: code.to_string() }))
    })
}

pub async fn get_video_by_token(
    State(state): State<AppState>,
    Path(share_token): Path<String>,
) -> Result<Json<VideoByTokenResponse>, (StatusCode, Json<ErrorResponse>)> {
    let input = get_video_by_token::Input { share_token };

    state.get_video_by_token.execute(input).await.map(|out| {
        Json(VideoByTokenResponse {
            title: out.title,
            stream_url: out.stream_url,
        })
    }).map_err(|e| {
        let (status, code) = match &e {
            get_video_by_token::Error::VideoNotFound => (StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND"),
            get_video_by_token::Error::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        };
        (status, Json(ErrorResponse { error: e.to_string(), code: code.to_string() }))
    })
}
