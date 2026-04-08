//! Streams HLS output to object storage *while* the GStreamer pipeline
//! is still running, instead of waiting for the whole transcode to
//! finish. This is what gives us time-to-stream on the order of
//! "probe + a handful of seconds" rather than "length of the full
//! encode". It also owns the "first-segment" trigger: the moment the
//! low tier's first segment is durably uploaded, it generates and
//! uploads the master playlist and fires the caller's
//! [`FirstSegmentNotifier`], so the share link can be published long
//! before medium/high finish.
//!
//! Design sketch:
//!
//! - The GStreamer pipeline writes segments and per-tier playlists into
//!   a local temp directory (one subdirectory per quality level).
//! - This uploader runs concurrently in a tokio task, polling the temp
//!   dir every [`POLL_INTERVAL`] and diffing against what it has
//!   already uploaded.
//! - Segments are uploaded in filename order. The most recent segment
//!   in each tier is held back on normal ticks because hlssink2 may
//!   still be writing to it; a final drain after the pipeline stops
//!   picks up those last segments.
//! - Per-tier playlists are re-uploaded whenever hlssink2 rewrites them
//!   (detected by mtime). The final rewrite — the one that appends
//!   `#EXT-X-ENDLIST` — happens at EOS and is picked up by the drain.
//! - Master playlist generation is deterministic from the quality
//!   ladder and `has_audio`, so it's generated locally (no probing)
//!   and uploaded exactly once, the first tick after the low tier has
//!   a segment on S3. At that same moment we also force-upload
//!   `low/playlist.m3u8` if it exists, so a viewer following the
//!   share link sees master + the low variant playlist + at least
//!   one segment all in place, not master alone with 404'd variants.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{FirstSegmentNotifier, TranscoderError};
use tokio::sync::oneshot;

use super::quality::QualityLevel;

/// How often the uploader scans the temp dir for new files. 500 ms is
/// short enough that the low tier's first segment is published within
/// a second of landing on disk, and long enough to keep idle CPU at
/// effectively zero.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

const SEGMENT_EXTENSION: &str = "ts";
const PLAYLIST_FILENAME: &str = "playlist.m3u8";
const MASTER_FILENAME: &str = "master.m3u8";
const TS_CONTENT_TYPE: &str = "video/mp2t";
const HLS_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";

/// Runs in the background while the GStreamer pipeline transcodes.
/// See module docs for the overall design; construct with
/// [`HlsUploader::new`] and drive with [`HlsUploader::run`].
pub(super) struct HlsUploader {
    temp_root: PathBuf,
    storage: Arc<dyn StoragePort>,
    output_prefix: String,
    quality_levels: Vec<QualityLevel>,
    has_audio: bool,
    notifier: Option<Box<dyn FirstSegmentNotifier>>,
    state: UploadState,
}

/// Per-run bookkeeping. Lives inside [`HlsUploader`] so the public
/// surface stays small.
#[derive(Default)]
struct UploadState {
    /// Local segment paths already uploaded to S3. Tracked as
    /// `HashSet<PathBuf>` so the "already uploaded, skip" check in
    /// [`HlsUploader::upload_one_segment`] is O(1), and so a failed
    /// local `remove_file` doesn't cause a re-upload on the next tick.
    uploaded_segments: HashSet<PathBuf>,
    /// Last-observed mtime of each tier's `playlist.m3u8`. We re-upload
    /// only when this changes, so a healthy pipeline with 30 ticks /
    /// minute doesn't spam S3 with identical ~300-byte playlists.
    playlist_mtimes: std::collections::HashMap<PathBuf, SystemTime>,
    /// Flips to `true` the instant the first low-tier segment has
    /// finished uploading to storage. Used as the gating condition
    /// for master + notifier publication — kept as a plain bool so
    /// the trigger can't be accidentally broken by a path-matching
    /// bug on the uploaded-segments set.
    first_low_segment_uploaded: bool,
    /// Whether `master.m3u8` has been written and uploaded. Set
    /// exactly once, in the same tick as `first_low_segment_uploaded`.
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

    /// Poll loop. Returns when the caller signals stop via `stop_rx`
    /// or when a storage error aborts the upload.
    ///
    /// The final tick after the stop signal runs with
    /// `include_most_recent_segments = true`, which picks up the last
    /// segment of each tier (which the normal tick holds back because
    /// hlssink2 may still be writing it) plus the final playlist
    /// rewrite that contains `#EXT-X-ENDLIST`.
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
        // Backstop publish in case `upload_one_segment` didn't hit
        // the inline path (e.g. the final-drain tick after a failure
        // where the first low segment landed but the inline publish
        // hit a transient error). Cheap no-op once `master_uploaded`
        // is set.
        self.publish_master_if_ready().await?;
        Ok(())
    }

    /// Upload any new segments and the variant playlist for a single
    /// quality level. On [`TickMode::Running`] the most recent segment
    /// is held back (hlssink2 may still be writing it); on
    /// [`TickMode::FinalDrain`] everything gets uploaded.
    async fn upload_tier(
        &mut self,
        level: QualityLevel,
        mode: TickMode,
    ) -> Result<(), TranscoderError> {
        let tier_dir = self.temp_root.join(level.name());
        if !tier_dir.exists() {
            return Ok(());
        }

        let scan = scan_tier_dir(&tier_dir)?;
        let uploadable = select_uploadable_segments(&scan.segments, mode);

        for segment_path in uploadable {
            self.upload_one_segment(segment_path, level).await?;
        }

        if let Some(playlist_path) = scan.playlist {
            self.upload_playlist_if_changed(&playlist_path, level).await?;
        }

        Ok(())
    }

    async fn upload_one_segment(
        &mut self,
        segment_path: &Path,
        level: QualityLevel,
    ) -> Result<(), TranscoderError> {
        if self.state.uploaded_segments.contains(segment_path) {
            return Ok(());
        }

        let filename = match segment_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => return Ok(()),
        };
        let key = tier_object_key(&self.output_prefix, level, filename);

        self.storage
            .upload_from_path(segment_path, &key, TS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload segment: {e}")))?;

        // Remove the local file so long runs don't pile up gigabytes
        // of already-uploaded segments in the temp dir. Ignore the
        // result — a failure here means the next tick's read_dir will
        // skip it via `uploaded_segments`, not re-upload.
        let _ = tokio::fs::remove_file(segment_path).await;

        self.state
            .uploaded_segments
            .insert(segment_path.to_path_buf());

        // First-low-segment gate: publish master + fire the share-link
        // notifier *inline*, right here in the same `upload_one_segment`
        // call. It is tempting to defer this to the end of the tick —
        // but a single tick walks all three tiers sequentially and each
        // upload can take tens of seconds on a loaded encoder, so the
        // "end of tick" moment is effectively "end of the transcode",
        // i.e. way too late for a time-to-share improvement. Publishing
        // inline means the share link goes out within a few hundred
        // milliseconds of seg0 landing in storage, while the rest of
        // the tick (seg1, medium, high) continues afterwards.
        if level == QualityLevel::Low && !self.state.first_low_segment_uploaded {
            self.state.first_low_segment_uploaded = true;
            tracing::info!(
                segment = %filename,
                "first low-tier segment uploaded; publishing share link inline",
            );
            self.publish_master_if_ready().await?;
        }

        Ok(())
    }

    async fn upload_playlist_if_changed(
        &mut self,
        playlist_path: &Path,
        level: QualityLevel,
    ) -> Result<(), TranscoderError> {
        let mtime = match current_mtime(playlist_path) {
            Some(m) => m,
            None => return Ok(()),
        };
        if self.state.playlist_mtimes.get(playlist_path) == Some(&mtime) {
            return Ok(());
        }
        self.upload_playlist_now(playlist_path, level).await?;
        self.state
            .playlist_mtimes
            .insert(playlist_path.to_path_buf(), mtime);
        Ok(())
    }

    /// Upload a variant playlist *now*, bypassing the mtime guard.
    /// Used when we publish the master playlist: we want to guarantee
    /// `low/playlist.m3u8` is in storage alongside `master.m3u8` so
    /// the player's first variant fetch doesn't 404 while waiting for
    /// hlssink2 to flush an updated playlist on its own schedule.
    async fn upload_playlist_now(
        &self,
        playlist_path: &Path,
        level: QualityLevel,
    ) -> Result<(), TranscoderError> {
        let key = tier_object_key(&self.output_prefix, level, PLAYLIST_FILENAME);
        self.storage
            .upload_from_path(playlist_path, &key, HLS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload playlist: {e}")))?;
        Ok(())
    }

    async fn publish_master_if_ready(&mut self) -> Result<(), TranscoderError> {
        if self.state.master_uploaded {
            return Ok(());
        }
        if !self.state.first_low_segment_uploaded {
            return Ok(());
        }

        tracing::info!("publishing master playlist and firing share-link notifier");

        // Force the current low variant playlist to storage at the
        // same moment, so the player can resolve the first variant
        // fetch. If the local file doesn't exist yet (hlssink2 hasn't
        // flushed it), we log and carry on — the viewer's player
        // will retry the variant fetch while the regular mtime path
        // eventually uploads it a tick or two later.
        self.upload_low_playlist_if_present().await?;

        self.write_and_upload_master().await?;
        self.state.master_uploaded = true;
        self.fire_notifier_once();
        Ok(())
    }

    async fn upload_low_playlist_if_present(&mut self) -> Result<(), TranscoderError> {
        let low_playlist = self
            .temp_root
            .join(QualityLevel::Low.name())
            .join(PLAYLIST_FILENAME);
        if !low_playlist.exists() {
            tracing::warn!(
                path = %low_playlist.display(),
                "low/playlist.m3u8 not on disk yet at master publish; player may briefly 404",
            );
            return Ok(());
        }
        self.upload_playlist_now(&low_playlist, QualityLevel::Low).await?;
        if let Some(mtime) = current_mtime(&low_playlist) {
            self.state.playlist_mtimes.insert(low_playlist, mtime);
        }
        Ok(())
    }

    async fn write_and_upload_master(&self) -> Result<(), TranscoderError> {
        let content = generate_master_playlist(&self.quality_levels, self.has_audio);
        let master_path = self.temp_root.join(MASTER_FILENAME);

        tokio::fs::write(&master_path, &content)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("write master: {e}")))?;

        let key = format!("{}{}", self.output_prefix, MASTER_FILENAME);
        self.storage
            .upload_from_path(&master_path, &key, HLS_CONTENT_TYPE)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload master: {e}")))?;

        let _ = tokio::fs::remove_file(&master_path).await;
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
    /// including the last segment of each tier and the final playlist
    /// rewrite containing `#EXT-X-ENDLIST`.
    FinalDrain,
}

/// Snapshot of a single tier's directory contents — separated out so
/// [`HlsUploader::upload_tier`] doesn't have to juggle multiple return
/// values from an inline read.
struct TierScan {
    segments: Vec<PathBuf>,
    playlist: Option<PathBuf>,
}

fn scan_tier_dir(dir: &Path) -> Result<TierScan, TranscoderError> {
    let mut segments: Vec<PathBuf> = Vec::new();
    let mut playlist: Option<PathBuf> = None;

    let entries = std::fs::read_dir(dir)
        .map_err(|e| TranscoderError::TranscodeFailed(format!("readdir {:?}: {e}", dir)))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == PLAYLIST_FILENAME {
            playlist = Some(path);
        } else if path.extension().and_then(|e| e.to_str()) == Some(SEGMENT_EXTENSION) {
            segments.push(path);
        }
    }

    // Filename order matches temporal order (segment_00000.ts,
    // segment_00001.ts, ...) so sorting here gives us chronological
    // upload order with no extra parsing.
    segments.sort();
    Ok(TierScan { segments, playlist })
}

/// Pick which of the scanned segments are safe to upload this tick.
/// On a running tick, hold back the most recent segment since
/// hlssink2 may still be appending to it. On the final drain, take
/// everything.
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

fn current_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Build the HLS master playlist.
///
/// Includes a `CODECS=` attribute on each variant so the player can
/// pick a variant from the master alone (no need to fetch every
/// per-tier playlist first to discover codecs). The codec string is
/// stable across tiers because the pipeline pins H.264 to high
/// profile, level 4.0 — see the `h264_caps` capsfilter in
/// `gstreamer.rs::run_parallel_pipeline`.
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
