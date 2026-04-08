use chrono::Utc;

use domain::video::{
    generate_share_token, Video, VideoFormat, VideoId, VideoStatus, SHARE_TOKEN_LENGTH,
};

fn make_video(status: VideoStatus, share_token: Option<&str>) -> Video {
    Video {
        id: VideoId::new(),
        share_token: share_token.map(|s| s.to_string()),
        title: "t".into(),
        format: VideoFormat::Mp4,
        status,
        upload_key: "uploads/x/original.mp4".into(),
        created_at: Utc::now(),
    }
}

// ---- VideoFormat::from_mime_type ----

#[test]
fn mime_type_maps_to_every_supported_format() {
    assert_eq!(VideoFormat::from_mime_type("video/mp4"), Some(VideoFormat::Mp4));
    assert_eq!(VideoFormat::from_mime_type("video/webm"), Some(VideoFormat::Webm));
    assert_eq!(VideoFormat::from_mime_type("video/quicktime"), Some(VideoFormat::Mov));
    assert_eq!(VideoFormat::from_mime_type("video/x-msvideo"), Some(VideoFormat::Avi));
    assert_eq!(VideoFormat::from_mime_type("video/x-matroska"), Some(VideoFormat::Mkv));
}

#[test]
fn mime_type_unsupported_returns_none() {
    assert_eq!(VideoFormat::from_mime_type("image/png"), None);
    assert_eq!(VideoFormat::from_mime_type("text/plain"), None);
    assert_eq!(VideoFormat::from_mime_type(""), None);
}

// ---- VideoFormat::as_str / from_str round-trip ----

#[test]
fn video_format_string_round_trip() {
    for fmt in [
        VideoFormat::Mp4,
        VideoFormat::Webm,
        VideoFormat::Mov,
        VideoFormat::Avi,
        VideoFormat::Mkv,
    ] {
        assert_eq!(VideoFormat::from_str(fmt.as_str()), Some(fmt));
    }
}

#[test]
fn video_format_from_str_unknown_returns_none() {
    assert_eq!(VideoFormat::from_str("GIF"), None);
}

// ---- VideoFormat::extension ----

#[test]
fn video_format_extensions() {
    assert_eq!(VideoFormat::Mp4.extension(), ".mp4");
    assert_eq!(VideoFormat::Webm.extension(), ".webm");
    assert_eq!(VideoFormat::Mov.extension(), ".mov");
    assert_eq!(VideoFormat::Avi.extension(), ".avi");
    assert_eq!(VideoFormat::Mkv.extension(), ".mkv");
}

// ---- VideoFormat::validate_signature ----

#[test]
fn mp4_signature_requires_ftyp_at_offset_4() {
    // MP4: bytes 4..8 must be "ftyp"
    let mut bytes = [0u8; 16];
    bytes[4..8].copy_from_slice(b"ftyp");
    assert!(VideoFormat::Mp4.validate_signature(&bytes));
    assert!(VideoFormat::Mov.validate_signature(&bytes));
}

#[test]
fn mp4_signature_rejects_missing_ftyp() {
    let bytes = [0u8; 16];
    assert!(!VideoFormat::Mp4.validate_signature(&bytes));
}

#[test]
fn webm_mkv_signature_requires_ebml_header() {
    // EBML magic: 0x1A 0x45 0xDF 0xA3
    let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3];
    bytes.extend_from_slice(&[0u8; 16]);
    assert!(VideoFormat::Webm.validate_signature(&bytes));
    assert!(VideoFormat::Mkv.validate_signature(&bytes));
}

#[test]
fn webm_rejects_wrong_magic() {
    let bytes = [0u8; 16];
    assert!(!VideoFormat::Webm.validate_signature(&bytes));
}

#[test]
fn avi_signature_requires_riff() {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&[0u8; 16]);
    assert!(VideoFormat::Avi.validate_signature(&bytes));
}

#[test]
fn validate_signature_rejects_short_input() {
    // Less than 12 bytes is always rejected
    let bytes = b"RIFF".to_vec();
    assert!(!VideoFormat::Avi.validate_signature(&bytes));
}

// ---- VideoStatus::from_str ----

#[test]
fn video_status_string_round_trip() {
    for status in [
        VideoStatus::PendingUpload,
        VideoStatus::Uploaded,
        VideoStatus::Processing,
        VideoStatus::Processed,
        VideoStatus::Failed,
    ] {
        assert_eq!(VideoStatus::from_str(status.as_str()), Some(status));
    }
}

#[test]
fn video_status_from_str_unknown_returns_none() {
    assert_eq!(VideoStatus::from_str("UNKNOWN"), None);
}

// ---- Video::share_url ----

#[test]
fn share_url_none_when_no_token() {
    let video = make_video(VideoStatus::Processed, None);
    assert_eq!(video.share_url("http://example.com"), None);
}

#[test]
fn share_url_formatted_when_token_present() {
    let video = make_video(VideoStatus::Processed, Some("abc123"));
    assert_eq!(
        video.share_url("http://example.com"),
        Some("http://example.com/v/abc123".to_string())
    );
}

// ---- Video::is_streamable ----

#[test]
fn only_processed_is_streamable() {
    assert!(make_video(VideoStatus::Processed, None).is_streamable());
    assert!(!make_video(VideoStatus::PendingUpload, None).is_streamable());
    assert!(!make_video(VideoStatus::Uploaded, None).is_streamable());
    assert!(!make_video(VideoStatus::Processing, None).is_streamable());
    assert!(!make_video(VideoStatus::Failed, None).is_streamable());
}

// ---- Video::storage_prefix ----

#[test]
fn storage_prefix_includes_video_id() {
    let video = make_video(VideoStatus::Processed, None);
    let prefix = video.storage_prefix();
    assert!(prefix.starts_with("videos/"));
    assert!(prefix.ends_with('/'));
    assert!(prefix.contains(&video.id.0.to_string()));
}

// ---- Video::stream_url ----

#[test]
fn stream_url_none_when_not_streamable() {
    let video = make_video(VideoStatus::Processing, None);
    assert_eq!(video.stream_url("http://cdn.example.com"), None);
}

#[test]
fn stream_url_points_at_master_playlist() {
    let video = make_video(VideoStatus::Processed, None);
    let url = video.stream_url("http://cdn.example.com").unwrap();
    assert!(url.starts_with("http://cdn.example.com/videos/"));
    assert!(url.ends_with("/master.m3u8"));
}

// ---- generate_share_token ----

#[test]
fn share_token_has_expected_length() {
    let token = generate_share_token();
    assert_eq!(token.chars().count(), SHARE_TOKEN_LENGTH);
}

#[test]
fn share_tokens_are_unique() {
    let a = generate_share_token();
    let b = generate_share_token();
    assert_ne!(a, b);
}

// ---- VideoId ----

#[test]
fn video_ids_are_unique() {
    let a = VideoId::new();
    let b = VideoId::new();
    assert_ne!(a, b);
}
