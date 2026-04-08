use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_UPLOAD_SIZE_BYTES: i64 = 1_073_741_824; // 1 GB
pub const MAX_TITLE_LENGTH: usize = 100;
pub const SHARE_TOKEN_LENGTH: usize = 21;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoId(pub Uuid);

impl VideoId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for VideoId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for VideoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: VideoId,
    pub share_token: Option<String>,
    pub title: String,
    pub format: VideoFormat,
    pub status: VideoStatus,
    pub upload_key: String,
    pub created_at: DateTime<Utc>,
}

impl Video {
    pub fn share_url(&self, base_url: &str) -> Option<String> {
        self.share_token
            .as_ref()
            .map(|token| format!("{}/v/{}", base_url, token))
    }

    pub fn is_streamable(&self) -> bool {
        matches!(self.status, VideoStatus::Processed)
    }

    pub fn storage_prefix(&self) -> String {
        format!("videos/{}/", self.id.0)
    }

    pub fn stream_url(&self, cdn_base_url: &str) -> Option<String> {
        if self.is_streamable() {
            Some(format!("{}/{}master.m3u8", cdn_base_url, self.storage_prefix()))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoStatus {
    PendingUpload,
    Uploaded,
    Processing,
    Processed,
    Failed,
}

impl VideoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PendingUpload => "PENDING_UPLOAD",
            Self::Uploaded => "UPLOADED",
            Self::Processing => "PROCESSING",
            Self::Processed => "PROCESSED",
            Self::Failed => "FAILED",
        }
    }

    // Returns Option (not Result like std FromStr) because the only
    // caller is the row mapper, which panics on None — there is no
    // useful error value to wrap and propagate.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "PENDING_UPLOAD" => Some(Self::PendingUpload),
            "UPLOADED" => Some(Self::Uploaded),
            "PROCESSING" => Some(Self::Processing),
            "PROCESSED" => Some(Self::Processed),
            "FAILED" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoFormat {
    Mp4,
    Webm,
    Mov,
    Avi,
    Mkv,
}

impl VideoFormat {
    pub fn from_mime_type(mime: &str) -> Option<Self> {
        match mime {
            "video/mp4" => Some(Self::Mp4),
            "video/webm" => Some(Self::Webm),
            "video/quicktime" => Some(Self::Mov),
            "video/x-msvideo" => Some(Self::Avi),
            "video/x-matroska" => Some(Self::Mkv),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Webm => "WEBM",
            Self::Mov => "MOV",
            Self::Avi => "AVI",
            Self::Mkv => "MKV",
        }
    }

    // Returns Option (not Result like std FromStr) for the same reason
    // as VideoStatus::from_str above — caller wants a panic-on-None
    // semantic, not an error value to propagate.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "MP4" => Some(Self::Mp4),
            "WEBM" => Some(Self::Webm),
            "MOV" => Some(Self::Mov),
            "AVI" => Some(Self::Avi),
            "MKV" => Some(Self::Mkv),
            _ => None,
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => ".mp4",
            Self::Webm => ".webm",
            Self::Mov => ".mov",
            Self::Avi => ".avi",
            Self::Mkv => ".mkv",
        }
    }

    /// Magic bytes pattern for file signature validation.
    pub fn validate_signature(&self, bytes: &[u8]) -> bool {
        if bytes.len() < 12 {
            return false;
        }
        match self {
            Self::Mp4 | Self::Mov => &bytes[4..8] == b"ftyp",
            Self::Webm | Self::Mkv => bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
            Self::Avi => bytes.starts_with(b"RIFF"),
        }
    }
}

pub fn generate_share_token() -> String {
    nanoid::nanoid!(SHARE_TOKEN_LENGTH)
}

