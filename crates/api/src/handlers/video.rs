use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use application::usecases::video::{
    complete_upload, get_video_by_token, get_video_status, initiate_upload,
};
use domain::video::VideoId;

use crate::AppState;

// ---- Request / response DTOs ----

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

/// HTTP error reply carrying a status code and a structured JSON body.
pub struct ApiError {
    status: StatusCode,
    body: ErrorResponse,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorResponse {
                error: message.into(),
                code: code.to_string(),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

// ---- Use-case error → API error mappings ----

impl From<initiate_upload::Error> for ApiError {
    fn from(e: initiate_upload::Error) -> Self {
        use initiate_upload::Error::*;
        match &e {
            TitleRequired => ApiError::new(StatusCode::BAD_REQUEST, "TITLE_REQUIRED", e.to_string()),
            TitleTooLong => ApiError::new(StatusCode::BAD_REQUEST, "TITLE_TOO_LONG", e.to_string()),
            UnsupportedFormat => {
                ApiError::new(StatusCode::BAD_REQUEST, "UNSUPPORTED_FORMAT", e.to_string())
            }
            Internal(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            ),
        }
    }
}

impl From<complete_upload::Error> for ApiError {
    fn from(e: complete_upload::Error) -> Self {
        use complete_upload::Error::*;
        match &e {
            VideoNotFound => ApiError::new(StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND", e.to_string()),
            AlreadyCompleted => ApiError::new(StatusCode::CONFLICT, "ALREADY_COMPLETED", e.to_string()),
            FileNotFoundInStorage => ApiError::new(
                StatusCode::NOT_FOUND,
                "FILE_NOT_FOUND_IN_STORAGE",
                e.to_string(),
            ),
            FileTooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "FILE_TOO_LARGE",
                e.to_string(),
            ),
            InvalidFileSignature => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_FILE_SIGNATURE",
                e.to_string(),
            ),
            Internal(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            ),
        }
    }
}

impl From<get_video_status::Error> for ApiError {
    fn from(e: get_video_status::Error) -> Self {
        use get_video_status::Error::*;
        match &e {
            VideoNotFound => ApiError::new(StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND", e.to_string()),
            Internal(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            ),
        }
    }
}

impl From<get_video_by_token::Error> for ApiError {
    fn from(e: get_video_by_token::Error) -> Self {
        use get_video_by_token::Error::*;
        match &e {
            VideoNotFound => ApiError::new(StatusCode::NOT_FOUND, "VIDEO_NOT_FOUND", e.to_string()),
            Internal(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            ),
        }
    }
}

/// Parses a path segment as a UUID, returning a structured JSON 400 instead
/// of Axum's plain-text rejection.
fn parse_uuid(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_VIDEO_ID",
            format!("'{}' is not a valid video id", raw),
        )
    })
}

// ---- Handlers ----

pub async fn initiate_upload(
    State(state): State<AppState>,
    Json(body): Json<InitiateUploadRequest>,
) -> Result<Json<InitiateUploadResponse>, ApiError> {
    let input = initiate_upload::Input {
        title: body.title,
        mime_type: body.mime_type,
    };

    let out = state.initiate_upload.execute(input).await?;
    Ok(Json(InitiateUploadResponse {
        id: out.id.0,
        upload_url: out.upload_url,
    }))
}

pub async fn complete_upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<CompleteUploadResponse>, ApiError> {
    let id = parse_uuid(&id)?;
    let input = complete_upload::Input { id: VideoId(id) };

    let out = state.complete_upload.execute(input).await?;
    Ok(Json(CompleteUploadResponse {
        id: out.id.0,
        status: out.status.as_str().to_string(),
    }))
}

pub async fn get_video_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VideoStatusResponse>, ApiError> {
    let id = parse_uuid(&id)?;
    let input = get_video_status::Input { id: VideoId(id) };

    let out = state.get_video_status.execute(input).await?;
    Ok(Json(VideoStatusResponse {
        status: out.status.as_str().to_string(),
        share_url: out.share_url,
    }))
}

pub async fn get_video_by_token(
    State(state): State<AppState>,
    Path(share_token): Path<String>,
) -> Result<Json<VideoByTokenResponse>, ApiError> {
    let input = get_video_by_token::Input { share_token };

    let out = state.get_video_by_token.execute(input).await?;
    Ok(Json(VideoByTokenResponse {
        title: out.title,
        stream_url: out.stream_url,
    }))
}
