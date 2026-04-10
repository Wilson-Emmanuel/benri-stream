//! Uploads HLS segments to object storage while the GStreamer pipeline runs,
//! cutting time-to-stream from "full encode duration" to "probe + a few seconds".
//!
//! A background task polls the temp dir at [`POLL_INTERVAL`]. For each tier it
//! uploads all completed segments (holding back the most recent, which hlssink2
//! may still be writing), then regenerates the variant playlist from the
//! uploaded-segment list. Playlists are synthesized from what's in S3 — hlssink2's
//! on-disk playlist is ignored — so the S3 state is always authoritative.
//!
//! When the low tier's first segment lands in storage, the master playlist is
//! uploaded and [`FirstSegmentNotifier`] is fired so the share link can be
//! published immediately. On final drain each variant playlist is closed with
//! `#EXT-X-ENDLIST`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{FirstSegmentNotifier, TranscoderError};
use tokio::sync::oneshot;

use super::quality::QualityLevel;

/// Scan interval. Short enough to publish the first segment within ~1 s of
/// it landing on disk; long enough that idle CPU usage is negligible.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

const SEGMENT_EXTENSION: &str = "ts";
const MASTER_FILENAME: &str = "master.m3u8";
const PLAYLIST_FILENAME: &str = "playlist.m3u8";
const TS_CONTENT_TYPE: &str = "video/mp2t";
const HLS_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";

/// Background upload task. See module docs for design; drive with [`HlsUploader::run`].
pub(super) struct HlsUploader {
    temp_root: PathBuf,
    storage: Arc<dyn StoragePort>,
    output_prefix: String,
    quality_levels: Vec<QualityLevel>,
    has_audio: bool,
    notifier: Option<Box<dyn FirstSegmentNotifier>>,
    state: UploadState,
}

/// Per-run upload bookkeeping.
#[derive(Default)]
struct UploadState {
    /// Ordered filenames of segments uploaded per tier. The synthesized
    /// variant playlist references exactly this list — it matches what is
    /// in S3 by construction.
    uploaded_segments: HashMap<QualityLevel, Vec<String>>,
    /// Last playlist body uploaded per tier, used to skip redundant uploads.
    last_published_playlist: HashMap<QualityLevel, String>,
    /// Set when the first low-tier segment has been durably uploaded.
    /// Gates master playlist publication and the notifier call.
    first_low_segment_uploaded: bool,
    /// Whether `master.m3u8` has been uploaded. Set exactly once,
    /// in the same tick as `first_low_segment_uploaded`.
    master_uploaded: bool,
}

impl HlsUploader {
    pub(super) fn new(
        temp_root: PathBuf,
        storage: Arc<dyn StoragePort>,
        output_prefix: String,
        quality_levels: Vec<QualityLevel>,
        has_audio: bool,
        notifier: Box<dyn FirstSegmentNotifier>,
    ) -> Self {
        Self {
            temp_root,
            storage,
            output_prefix,
            quality_levels,
            has_audio,
            notifier: Some(notifier),
            state: UploadState::default(),
        }
    }

    /// Poll loop. Returns on `stop_rx` signal (after a final drain) or on
    /// storage error. The final drain uploads the last held-back segment of
    /// each tier and closes each variant playlist with `#EXT-X-ENDLIST`.
    pub(super) async fn run(
        mut self,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<(), TranscoderError> {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {
                    self.tick(TickMode::Running).await?;
                }
                _ = &mut stop_rx => {
                    self.tick(TickMode::FinalDrain).await?;
                    return Ok(());
                }
            }
        }
    }

    async fn tick(&mut self, mode: TickMode) -> Result<(), TranscoderError> {
        for level in self.quality_levels.clone() {
            self.upload_tier(level, mode).await?;
        }
        Ok(())
    }

    /// Upload any new segments for `level`, republishing the variant playlist
    /// after each segment so S3 is never ahead of the playlist. On
    /// [`TickMode::Running`] the most recent segment is held back (hlssink2
    /// may still be writing it). On [`TickMode::FinalDrain`] everything is
    /// uploaded and the playlist is finalized with `#EXT-X-ENDLIST`.
    ///
    /// Per-segment publishing (rather than end-of-tick) prevents a stale
    /// playlist window when a tick uploads multiple segments sequentially.
    async fn upload_tier(
        &mut self,
        level: QualityLevel,
        mode: TickMode,
    ) -> Result<(), TranscoderError> {
        let tier_dir = self.temp_root.join(level.name());
        if !tier_dir.exists() {
            return Ok(());
        }

        let on_disk = scan_segments(&tier_dir)?;
        let uploadable = select_uploadable_segments(&on_disk, mode);
        // Mid-loop publishes always use Running mode: publishing FinalDrain
        // here would attach #EXT-X-ENDLIST to a partial playlist before all
        // drain segments have landed. The publish after the loop uses the
        // real mode so finalization happens exactly once.
        for segment_path in uploadable {
            self.upload_one_segment(segment_path, level).await?;
            self.publish_variant_playlist_if_changed(level, TickMode::Running)
                .await?;
        }

        // Final publish with the real mode. On a drain tick this flips
        // the playlist to its finalized form; the content comparison inside
        // detects the EVENT → ENDLIST transition even when no new segments
        // were uploaded.
        self.publish_variant_playlist_if_changed(level, mode).await?;
        Ok(())
    }

    async fn upload_one_segment(
        &mut self,
        segment_path: &Path,
        level: QualityLevel,
    ) -> Result<(), TranscoderError> {
        let filename = match segment_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => return Ok(()),
        };

        // Skip if already uploaded. Also handles the case where the local
        // delete below raced or failed and the file reappears on the next tick.
        if self
            .state
            .uploaded_segments
            .get(&level)
            .map(|v| v.iter().any(|n| n == &filename))
            .unwrap_or(false)
        {
            return Ok(());
        }

        let key = tier_object_key(&self.output_prefix, level, &filename);
        self.storage
            .upload_from_path(segment_path, &key, TS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload segment: {e}")))?;

        // Delete the local file to keep the temp dir from growing. A failure
        // here is benign: the next tick will find it again and skip it.
        let _ = tokio::fs::remove_file(segment_path).await;

        self.state
            .uploaded_segments
            .entry(level)
            .or_default()
            .push(filename.clone());

        tracing::info!(
            tier = level.name(),
            segment = %filename,
            total_uploaded_for_tier = self
                .state
                .uploaded_segments
                .get(&level)
                .map(|v| v.len())
                .unwrap_or(0),
            "uploaded segment",
        );

        // Publish the master and fire the notifier inline on the first low
        // segment, not at end-of-tick. Waiting for the rest of the tier
        // loop on a CPU-bound host is effectively end-of-transcode.
        if level == QualityLevel::Low && !self.state.first_low_segment_uploaded {
            self.state.first_low_segment_uploaded = true;
            tracing::info!(
                segment = %filename,
                "first low-tier segment uploaded; publishing share link inline",
            );
            self.publish_master_and_notify().await?;
        }

        Ok(())
    }

    /// Synthesize the variant playlist from uploaded segments and push to S3
    /// if the content changed. On [`TickMode::FinalDrain`] appends
    /// `#EXT-X-ENDLIST`.
    async fn publish_variant_playlist_if_changed(
        &mut self,
        level: QualityLevel,
        mode: TickMode,
    ) -> Result<(), TranscoderError> {
        let empty = Vec::new();
        let segments = self
            .state
            .uploaded_segments
            .get(&level)
            .unwrap_or(&empty);
        if segments.is_empty() {
            return Ok(());
        }

        let finalize = matches!(mode, TickMode::FinalDrain);
        let body = synthesize_variant_playlist(segments, finalize);
        let previous = self.state.last_published_playlist.get(&level);
        if previous == Some(&body) {
            return Ok(());
        }

        let key = tier_object_key(&self.output_prefix, level, PLAYLIST_FILENAME);
        self.storage
            .upload_bytes(&key, body.as_bytes(), HLS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload playlist: {e}")))?;
        tracing::info!(
            tier = level.name(),
            segments = segments.len(),
            finalized = finalize,
            "published variant playlist",
        );
        self.state.last_published_playlist.insert(level, body);
        Ok(())
    }

    async fn publish_master_and_notify(&mut self) -> Result<(), TranscoderError> {
        if self.state.master_uploaded {
            return Ok(());
        }
        tracing::info!("publishing master playlist and firing share-link notifier");

        // Publish the low variant playlist before the master so the player
        // can resolve its first variant fetch immediately. At this point
        // uploaded_segments[Low] has at least one entry.
        self.publish_variant_playlist_if_changed(QualityLevel::Low, TickMode::Running)
            .await?;

        let content = generate_master_playlist(&self.quality_levels, self.has_audio);
        let key = format!("{}{}", self.output_prefix, MASTER_FILENAME);
        self.storage
            .upload_bytes(&key, content.as_bytes(), HLS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload master: {e}")))?;

        self.state.master_uploaded = true;
        self.fire_notifier_once();
        Ok(())
    }

    fn fire_notifier_once(&mut self) {
        if let Some(notifier) = self.notifier.take() {
            notifier.notify();
        }
    }
}

#[derive(Copy, Clone)]
enum TickMode {
    /// Normal tick while the pipeline is still running. Holds back the
    /// most recent segment of each tier since it may still be open for
    /// writing by hlssink2.
    Running,
    /// Final tick after the pipeline has stopped. Uploads everything,
    /// including the last segment of each tier, and finalizes each
    /// tier's playlist with `#EXT-X-ENDLIST`.
    FinalDrain,
}

/// List `*.ts` files in a tier directory, sorted by filename.
/// Filename order equals temporal order for hlssink2's `segment_%05d.ts` pattern.
fn scan_segments(dir: &Path) -> Result<Vec<PathBuf>, TranscoderError> {
    let mut segments: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| TranscoderError::TranscodeFailed(format!("readdir {:?}: {e}", dir)))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(SEGMENT_EXTENSION) {
            segments.push(path);
        }
    }
    segments.sort();
    Ok(segments)
}

/// Returns the slice of segments safe to upload. On a running tick the most
/// recent segment is held back (hlssink2 may still be writing it); on the
/// final drain all segments are returned.
fn select_uploadable_segments(segments: &[PathBuf], mode: TickMode) -> &[PathBuf] {
    match mode {
        TickMode::FinalDrain => segments,
        TickMode::Running => match segments.len() {
            0 => segments,
            n => &segments[..n - 1],
        },
    }
}

fn tier_object_key(output_prefix: &str, level: QualityLevel, filename: &str) -> String {
    format!("{}{}/{}", output_prefix, level.name(), filename)
}

/// Build a minimal HLS media playlist listing `segment_filenames` in order.
/// When `finalize` is false the playlist is event-style so the player keeps
/// polling; when true `#EXT-X-ENDLIST` is appended. `#EXT-X-TARGETDURATION`
/// and `#EXTINF` values match the encoder's configured segment duration.
pub(super) fn synthesize_variant_playlist(
    segment_filenames: &[String],
    finalize: bool,
) -> String {
    let target = super::gstreamer::SEGMENT_DURATION_SECS;
    let mut playlist = String::new();
    playlist.push_str("#EXTM3U\n");
    playlist.push_str("#EXT-X-VERSION:3\n");
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target));
    playlist.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    if !finalize {
        playlist.push_str("#EXT-X-PLAYLIST-TYPE:EVENT\n");
    }
    for filename in segment_filenames {
        playlist.push_str(&format!("#EXTINF:{}.000,\n{}\n", target, filename));
    }
    if finalize {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

/// Build the HLS master playlist. Includes `CODECS=` on each variant so the
/// player can select a stream from the master alone. The codec string is
/// stable because the pipeline pins H.264 to high profile, level 4.0.
pub(super) fn generate_master_playlist(levels: &[QualityLevel], has_audio: bool) -> String {
    // avc1.640028 = High profile (64), no constraint flags (00),
    //               level 4.0 (28 hex = 40 dec).
    // mp4a.40.2   = AAC LC (object type 2 in MP4 audio object types).
    let codecs = if has_audio {
        "avc1.640028,mp4a.40.2"
    } else {
        "avc1.640028"
    };

    let mut m3u8 = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    for level in levels {
        let (width, height) = level.resolution();
        let bitrate = level.target_bitrate_bps();
        m3u8.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{},CODECS=\"{}\"\n{}/playlist.m3u8\n",
            bitrate,
            width,
            height,
            codecs,
            level.name()
        ));
    }
    m3u8
}
