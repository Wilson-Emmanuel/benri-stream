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
//! - The GStreamer pipeline writes segments into a local temp
//!   directory (one subdirectory per quality level). We do **not**
//!   use hlssink2's on-disk `playlist.m3u8` at all — we synthesize
//!   our own playlist from the set of segments we've actually
//!   uploaded to storage, and that synthesized playlist is the one
//!   the viewer's player fetches. This keeps the S3 state
//!   authoritative: the segments listed in the playlist are exactly
//!   the segments that exist on S3, by construction.
//! - A background tokio task polls the temp dir every
//!   [`POLL_INTERVAL`]. For each tier it uploads any newly-created
//!   segments (all but the most recent, which hlssink2 may still be
//!   writing), then regenerates the tier's variant playlist from the
//!   uploaded-segment list and uploads it too — but only when the
//!   playlist content actually changed.
//! - First-segment trigger: the moment the low tier's first segment
//!   is durably uploaded, we publish the master playlist (deterministic
//!   from the ladder + `has_audio`) and fire the caller's
//!   [`FirstSegmentNotifier`]. That same tick uploads the low variant
//!   playlist listing the single segment, so the viewer's player has
//!   something playable to attach to.
//! - On final drain (after the pipeline stops), each tier's playlist
//!   is regenerated with `#EXT-X-ENDLIST` appended so the player
//!   knows playback is complete and stops polling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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
const MASTER_FILENAME: &str = "master.m3u8";
const PLAYLIST_FILENAME: &str = "playlist.m3u8";
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
    /// Ordered filenames of segments we've uploaded for each tier.
    /// Used both for the "already uploaded" check and for
    /// synthesizing the variant playlist that goes to S3. The
    /// invariant is: whatever is listed in `uploaded_segments[tier]`
    /// is exactly what's in S3 under `output_prefix/tier/`, and that
    /// same list is what the synthesized playlist references.
    uploaded_segments: HashMap<QualityLevel, Vec<String>>,
    /// Last playlist content we uploaded for each tier, so a tick
    /// that produced no new segments skips re-uploading an identical
    /// playlist instead of spamming S3 with ~300 bytes per tick.
    last_published_playlist: HashMap<QualityLevel, String>,
    /// Flips to `true` the instant the first low-tier segment has
    /// finished uploading to storage. Used as the gating condition
    /// for master + notifier publication.
    first_low_segment_uploaded: bool,
    /// Whether `master.m3u8` has been written and uploaded. Set
    /// exactly once, in the same tick as
    /// `first_low_segment_uploaded`.
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
    /// [`TickMode::FinalDrain`], which picks up the last segment of
    /// each tier (which the normal tick holds back because hlssink2
    /// may still be writing it) and uploads each tier's variant
    /// playlist with `#EXT-X-ENDLIST` appended so the player knows
    /// to stop polling.
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

    /// Upload any new segments for `level`, republishing the tier's
    /// variant playlist after **each** one so it always matches the
    /// segments actually in S3. On [`TickMode::Running`] the most
    /// recent segment on disk is held back (hlssink2 may still be
    /// writing it); on [`TickMode::FinalDrain`] everything is
    /// uploaded and the playlist is finalized with `#EXT-X-ENDLIST`.
    ///
    /// Publishing per-segment (not just at end-of-tick) matters when
    /// a tick has multiple new segments to upload and each upload
    /// takes non-trivial time. End-of-tick publishing would leave the
    /// S3 playlist stale for the duration of the entire for loop,
    /// and a viewer polling during that window would never see the
    /// in-flight segments. The extra uploads are small (a few hundred
    /// bytes) and deduplicated by content, so the cost is negligible.
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
        // Intermediate publishes inside the loop always use
        // `Running` mode, even when the enclosing tick is a drain.
        // Publishing with `FinalDrain` mid-loop would attach
        // `#EXT-X-ENDLIST` to a partial playlist — a viewer polling
        // during the drain would see "playback complete, these N
        // segments are the whole stream" and stop, even though more
        // segments are still being uploaded after it.
        //
        // The single publish after the loop *is* allowed to use the
        // real mode, so the finalization happens exactly once —
        // after every segment the drain is going to upload has
        // actually landed in storage.
        for segment_path in uploadable {
            self.upload_one_segment(segment_path, level).await?;
            self.publish_variant_playlist_if_changed(level, TickMode::Running)
                .await?;
        }

        // Final publish with the *actual* mode — this is what flips
        // the playlist to its finalized form on a drain tick. Also
        // covers the drain-with-no-new-segments case: if the loop
        // above had nothing to do, we still publish here and the
        // content comparison inside will detect the `EVENT` →
        // `ENDLIST` transition and push the finalized version.
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

        // Already uploaded? Skip. This also dedupes cases where a
        // slow tick saw the same file twice because the local delete
        // below failed or raced a filesystem cache.
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

        // Remove the local file so long runs don't pile up gigabytes
        // of already-uploaded segments in the temp dir. Ignore the
        // result — a failure here means the next tick's scan will
        // find the file again, and the `already uploaded` check at
        // the top of this function will skip it.
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

        // First-low-segment gate: publish master + fire the share-link
        // notifier *inline*, in the same call. Publishing later (at
        // the end of the tick) would wait for the remaining low
        // segments + medium + high to upload, which on a CPU-bound
        // host is effectively "end of the transcode".
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

    /// Synthesize the variant playlist for `level` from the list of
    /// segments we've uploaded so far, and push it to S3 if it differs
    /// from what we pushed last time. On [`TickMode::FinalDrain`] the
    /// playlist is finalized with `#EXT-X-ENDLIST`.
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

        // Publish the low variant playlist inline too so the viewer's
        // player can resolve its first variant fetch without racing
        // the next tick. Content is derived from the uploaded-segment
        // list; by the time we're here `uploaded_segments[Low]` has
        // at least one filename (the segment we just uploaded).
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

/// List `*.ts` files in a tier directory, sorted by filename (which
/// equals temporal order for hlssink2's `segment_%05d.ts` pattern).
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

/// Build a minimal HLS media playlist listing `segment_filenames` in
/// order. When `finalize` is false, the playlist is event-style
/// (growing) so players continue polling for more segments; when
/// true, `#EXT-X-ENDLIST` is appended so the player knows playback
/// is complete and stops polling.
///
/// `#EXT-X-TARGETDURATION` and each `#EXTINF` are set to the encoder's
/// configured segment duration. Small discrepancies between the
/// declared and actual durations are tolerated by both hls.js and
/// native Safari HLS.
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

/// Build the HLS master playlist.
///
/// Includes a `CODECS=` attribute on each variant so the player can
/// pick a variant from the master alone (no need to fetch every
/// per-tier playlist first to discover codecs). The codec string is
/// stable across tiers because the pipeline pins H.264 to high
/// profile, level 4.0 — see the `h264_caps` capsfilter in
/// `gstreamer.rs::build_parallel_pipeline`.
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
