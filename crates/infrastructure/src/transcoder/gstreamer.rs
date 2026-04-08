use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{
    FirstSegmentNotifier, ProbeResult, TranscoderError, TranscoderPort,
};
use tokio::sync::oneshot;

use super::hls_uploader::HlsUploader;
use super::quality::QualityLevel;

/// Target HLS segment duration in seconds. Shorter = faster time-to-stream,
/// longer = fewer files and better CDN cache efficiency. 4s is the balance.
const SEGMENT_DURATION_SECS: u32 = 4;

/// Presigned-GET TTL for the input file. Sized to comfortably outlast
/// the task's `processing_timeout` (30 min for `ProcessVideoTaskMetadata`),
/// since the task system cancels anything running longer than that.
const INPUT_PRESIGN_TTL_SECS: u64 = 2 * 60 * 60;

/// GStreamer-based transcoder. Reads from S3 via presigned URL, writes HLS
/// segments to a local temp dir, and uploads them to S3 *while the
/// pipeline is still running* via a concurrent [`HlsUploader`] task.
/// Workers are stateless — nothing persists between jobs.
pub struct GstreamerTranscoder {
    storage: Arc<dyn StoragePort>,
}

impl GstreamerTranscoder {
    pub fn new(storage: Arc<dyn StoragePort>) -> Self {
        Self { storage }
    }

    /// Generate a time-limited presigned GET URL for reading the input
    /// file from storage. Presigned URLs (rather than the public CDN
    /// URL) keep `uploads/` private — only a worker holding a fresh
    /// presign can read the original file.
    ///
    /// `INPUT_PRESIGN_TTL_SECS` is sized to outlast probe (seconds) and
    /// the longest realistic transcode for a 1 GB source.
    async fn input_url(&self, storage_key: &str) -> Result<String, TranscoderError> {
        self.storage
            .generate_presigned_download_url(storage_key, INPUT_PRESIGN_TTL_SECS)
            .await
            .map_err(|e| {
                TranscoderError::TranscodeFailed(format!("presign download url: {e}"))
            })
    }

    /// Build and run a single HLS transcoding pipeline. The input is decoded
    /// once and fanned out to one encoder branch per quality level. If the
    /// source has an audio track, it's also decoded once, encoded once as
    /// AAC, and shared across all quality levels (audio is identical across
    /// tiers — no point re-encoding per tier).
    ///
    /// ```text
    /// uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale(360p)  → x264enc → h264parse ─┐
    ///                │                            ├─ queue → scale(720p)  → x264enc → h264parse ─┤
    ///                │                            └─ queue → scale(1080p) → x264enc → h264parse ─┤
    ///                │                                                                            ├──▶ hlssink2(per level, internal mpegtsmux)
    ///                └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ────────┘
    ///                   (only built if the source has an audio stream)
    /// ```
    ///
    /// The `queue` element after each tee src pad puts the branch on its own
    /// streaming thread — that's how the encoders actually run in parallel.
    ///
    /// `has_audio` is determined upstream by `probe()` and threaded in,
    /// avoiding a second header read from S3.
    fn run_parallel_pipeline(
        input_url: &str,
        output_dir: &Path,
        quality_levels: &[QualityLevel],
        has_audio: bool,
    ) -> Result<(), TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        use gstreamer_video as gst_video;

        let pipeline = gst::Pipeline::new();

        // Source: uridecodebin3 is the modern, streams-aware decode
        // element, stable since GStreamer 1.22 (this build targets 1.28).
        // It has more accurate HTTP buffering than the older
        // uridecodebin — relevant when reading from presigned S3 URLs
        // where over-downloading costs real egress.
        let src = gst::ElementFactory::make("uridecodebin3")
            .property("uri", input_url)
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("uridecodebin3: {e}")))?;

        // ---- Video front-end (always built) ----
        let video_convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("videoconvert: {e}")))?;
        let video_tee = gst::ElementFactory::make("tee")
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("video tee: {e}")))?;

        pipeline
            .add_many([&src, &video_convert, &video_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add video front-end: {e}")))?;
        gst::Element::link_many([&video_convert, &video_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("link video front-end: {e}")))?;

        // ---- Audio front-end (only if source has audio) ----
        // Encoded once and fanned out to all levels via audio_tee — audio
        // bitrate doesn't change across quality tiers, so re-encoding per
        // tier would be pure waste.
        let audio_chain = if has_audio {
            let audio_convert = gst::ElementFactory::make("audioconvert")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("audioconvert: {e}")))?;
            let audio_resample = gst::ElementFactory::make("audioresample")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("audioresample: {e}")))?;
            let audio_enc = gst::ElementFactory::make("avenc_aac")
                .property("bitrate", 128_000i32)
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("avenc_aac: {e}")))?;
            let audio_parse = gst::ElementFactory::make("aacparse")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("aacparse: {e}")))?;
            let audio_tee = gst::ElementFactory::make("tee")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("audio tee: {e}")))?;

            pipeline
                .add_many([
                    &audio_convert,
                    &audio_resample,
                    &audio_enc,
                    &audio_parse,
                    &audio_tee,
                ])
                .map_err(|e| TranscoderError::TranscodeFailed(format!("add audio front-end: {e}")))?;
            gst::Element::link_many([
                &audio_convert,
                &audio_resample,
                &audio_enc,
                &audio_parse,
                &audio_tee,
            ])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("link audio front-end: {e}")))?;

            Some((audio_convert, audio_tee))
        } else {
            None
        };

        // Dynamic pad linking: uridecodebin3 exposes decoded video/audio
        // pads after stream discovery. Route each to the right front-end.
        //
        // We route by pad *name* (`video_%u` / `audio_%u`) rather than by
        // caps because `uridecodebin3` exposes its source pads eagerly,
        // before caps negotiation completes — `pad.current_caps()` is
        // `None` at the moment `pad-added` fires. Pad names, by contrast,
        // are part of `uridecodebin3`'s template and are always set, so
        // they're a reliable type indicator at this point in the lifecycle.
        let video_convert_weak = video_convert.downgrade();
        let audio_convert_weak = audio_chain.as_ref().map(|(ac, _)| ac.downgrade());
        src.connect_pad_added(move |_, pad| {
            let pad_name = pad.name();

            if pad_name.starts_with("video_") {
                let Some(convert) = video_convert_weak.upgrade() else { return };
                let Some(sink_pad) = convert.static_pad("sink") else { return };
                if sink_pad.is_linked() {
                    // Already linked — ignore additional video streams.
                    return;
                }
                if let Err(e) = pad.link(&sink_pad) {
                    tracing::error!(error = %e, "failed to link video pad to videoconvert");
                }
            } else if pad_name.starts_with("audio_") {
                let Some(weak) = &audio_convert_weak else {
                    // We didn't build an audio chain (has_audio was false
                    // during discovery). Unusual but possible if streams
                    // changed between Discoverer and uridecodebin3.
                    return;
                };
                let Some(convert) = weak.upgrade() else { return };
                let Some(sink_pad) = convert.static_pad("sink") else { return };
                if sink_pad.is_linked() {
                    return;
                }
                if let Err(e) = pad.link(&sink_pad) {
                    tracing::error!(error = %e, "failed to link audio pad to audioconvert");
                }
            }
        });

        // Build one encoder branch per quality level.
        for level in quality_levels {
            let level_dir = output_dir.join(level.name());
            std::fs::create_dir_all(&level_dir)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("mkdir: {e}")))?;

            // ---- Video encoding branch ----
            let vqueue = gst::ElementFactory::make("queue")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("vqueue: {e}")))?;
            let vscale = gst::ElementFactory::make("videoscale")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("videoscale: {e}")))?;

            let (width, height) = level.resolution();
            let vcaps = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    gst_video::VideoCapsBuilder::new()
                        .width(width as i32)
                        .height(height as i32)
                        .build(),
                )
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("capsfilter: {e}")))?;

            let bitrate_kbps = level.target_bitrate_bps() / 1000;
            // `ultrafast` is the cheapest x264 preset — trades compression
            // efficiency for encode speed, roughly 3-5× throughput vs
            // `fast`. For this workload (three tiers encoding in parallel
            // on CPU, often on laptop-grade Docker hosts) wall-time is the
            // binding constraint, and the bitrate ceilings in the quality
            // ladder already bound the output size. Revisit if/when
            // hardware encoders become available (vtenc / nvenc / vaapi).
            let venc = gst::ElementFactory::make("x264enc")
                .property("bitrate", bitrate_kbps)
                .property_from_str("tune", "zerolatency")
                .property_from_str("speed-preset", "ultrafast")
                .property("key-int-max", SEGMENT_DURATION_SECS * 30)
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("x264enc: {e}")))?;

            // Pin H.264 profile=high, level=4.0 across all tiers. This
            // allows a stable CODECS="avc1.640028,..." attribute in the
            // master playlist, so the player can pick a variant from the
            // master alone instead of fetching every per-tier playlist
            // first. High@4.0 fits every output here (max 1080p30,
            // ≤25 Mbps).
            let h264_caps = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    gst::Caps::builder("video/x-h264")
                        .field("profile", "high")
                        .field("level", "4")
                        .build(),
                )
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("h264 capsfilter: {e}")))?;

            let vparse = gst::ElementFactory::make("h264parse")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("h264parse: {e}")))?;

            // ---- Per-level HLS sink ----
            // hlssink2 muxes to MPEG-TS internally and exposes request
            // pads named literally `video` and `audio`. We feed it the
            // already-parsed H.264 (and AAC, when present) elementary
            // streams directly — no external mpegtsmux needed.
            let playlist_path = level_dir.join("playlist.m3u8");
            let segment_pattern = level_dir.join("segment_%05d.ts");
            let hlssink = gst::ElementFactory::make("hlssink2")
                .property("target-duration", SEGMENT_DURATION_SECS)
                .property("playlist-length", 0u32)
                .property("playlist-location", playlist_path.to_str().unwrap())
                .property("location", segment_pattern.to_str().unwrap())
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("hlssink2: {e}")))?;

            pipeline
                .add_many([&vqueue, &vscale, &vcaps, &venc, &h264_caps, &vparse, &hlssink])
                .map_err(|e| TranscoderError::TranscodeFailed(format!("add level branch: {e}")))?;

            gst::Element::link_many([&vqueue, &vscale, &vcaps, &venc, &h264_caps, &vparse])
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link video chain: {e}")))?;

            // vparse → hlssink2.video (request pad)
            let hls_video_sink = hlssink
                .request_pad_simple("video")
                .ok_or_else(|| TranscoderError::TranscodeFailed("hlssink2 video pad".into()))?;
            let vparse_src = vparse
                .static_pad("src")
                .ok_or_else(|| TranscoderError::TranscodeFailed("vparse src pad".into()))?;
            vparse_src
                .link(&hls_video_sink)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link vparse→hlssink: {e}")))?;

            // video_tee → vqueue
            let video_tee_src = video_tee
                .request_pad_simple("src_%u")
                .ok_or_else(|| TranscoderError::TranscodeFailed("video tee src pad".into()))?;
            let vqueue_sink = vqueue
                .static_pad("sink")
                .ok_or_else(|| TranscoderError::TranscodeFailed("vqueue sink pad".into()))?;
            video_tee_src
                .link(&vqueue_sink)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link video tee: {e}")))?;

            // ---- Per-level audio branch (only if source has audio) ----
            if let Some((_, audio_tee_elem)) = &audio_chain {
                let aqueue = gst::ElementFactory::make("queue")
                    .build()
                    .map_err(|e| TranscoderError::TranscodeFailed(format!("aqueue: {e}")))?;
                pipeline
                    .add(&aqueue)
                    .map_err(|e| TranscoderError::TranscodeFailed(format!("add aqueue: {e}")))?;

                // audio_tee → aqueue
                let audio_tee_src = audio_tee_elem
                    .request_pad_simple("src_%u")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("audio tee src pad".into()))?;
                let aqueue_sink = aqueue
                    .static_pad("sink")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("aqueue sink pad".into()))?;
                audio_tee_src
                    .link(&aqueue_sink)
                    .map_err(|e| TranscoderError::TranscodeFailed(format!("link audio tee: {e}")))?;

                // aqueue → hlssink2.audio (request pad)
                let hls_audio_sink = hlssink
                    .request_pad_simple("audio")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("hlssink2 audio pad".into()))?;
                let aqueue_src = aqueue
                    .static_pad("src")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("aqueue src pad".into()))?;
                aqueue_src
                    .link(&hls_audio_sink)
                    .map_err(|e| TranscoderError::TranscodeFailed(format!("link aqueue→hlssink: {e}")))?;
            }
        }

        // Run the pipeline to completion
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("set playing: {e}")))?;

        let bus = pipeline.bus().unwrap();
        let mut error: Option<String> = None;

        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(err) => {
                    error = Some(format!("{} (debug: {:?})", err.error(), err.debug()));
                    break;
                }
                _ => {}
            }
        }

        pipeline.set_state(gst::State::Null).ok();

        if let Some(err_msg) = error {
            return Err(TranscoderError::TranscodeFailed(err_msg));
        }

        Ok(())
    }
}

#[async_trait]
impl TranscoderPort for GstreamerTranscoder {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError> {
        use gstreamer as gst;
        use gstreamer_pbutils as gst_pbutils;
        use gstreamer_pbutils::prelude::*;

        tracing::info!(storage_key, "transcoder: probing video file");

        let url = self.input_url(storage_key).await?;

        // Discoverer runs synchronously — use spawn_blocking to avoid blocking tokio
        tokio::task::spawn_blocking(move || {
            gst::init().map_err(|e| TranscoderError::ProbeFailed(format!("gst init: {e}")))?;

            let discoverer = gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(30))
                .map_err(|e| TranscoderError::ProbeFailed(format!("discoverer new: {e}")))?;

            let info = discoverer
                .discover_uri(&url)
                .map_err(|e| TranscoderError::ProbeFailed(format!("discover: {e}")))?;

            let duration_seconds = info
                .duration()
                .map(|d| d.nseconds() as f64 / 1_000_000_000.0)
                .unwrap_or(0.0);

            let video_streams = info.video_streams();
            let video = video_streams
                .first()
                .ok_or_else(|| TranscoderError::ProbeFailed("no video stream found".into()))?;

            let width = video.width();
            let height = video.height();
            let codec = video
                .caps()
                .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
                .unwrap_or_else(|| "unknown".into());

            let has_audio = !info.audio_streams().is_empty();

            Ok(ProbeResult {
                duration_seconds,
                width,
                height,
                codec,
                has_audio,
            })
        })
        .await
        .map_err(|e| TranscoderError::ProbeFailed(format!("task join: {e}")))?
    }

    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        probe: &ProbeResult,
        first_segment_ready: Box<dyn FirstSegmentNotifier>,
    ) -> Result<(), TranscoderError> {
        use gstreamer as gst;

        tracing::info!(
            input_key,
            output_prefix,
            has_audio = probe.has_audio,
            "transcoder: starting HLS transcode",
        );

        gst::init().map_err(|e| TranscoderError::TranscodeFailed(format!("gst init: {e}")))?;

        let input_url = self.input_url(input_key).await?;
        let temp_dir = tempfile::tempdir()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("tempdir: {e}")))?;
        let temp_path = temp_dir.path().to_path_buf();
        let quality_levels = QualityLevel::all().to_vec();

        // Kick off the uploader *before* the pipeline so it catches
        // segments from the moment they're written. It runs in its
        // own tokio task, polling the temp dir; see
        // `hls_uploader::HlsUploader` for the details. We signal it to
        // stop via `stop_tx` once the pipeline has left `Playing`.
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let uploader = HlsUploader::new(
            temp_path.clone(),
            self.storage.clone(),
            output_prefix.to_string(),
            quality_levels.clone(),
            probe.has_audio,
            first_segment_ready,
        );
        let uploader_handle = tokio::spawn(uploader.run(stop_rx));

        // GStreamer's bus iteration blocks the thread, so the pipeline
        // runs inside `spawn_blocking`. Run in parallel with the
        // uploader task above.
        let pipeline_result =
            Self::run_pipeline_blocking(input_url, temp_path, quality_levels, probe.has_audio)
                .await;

        // Tell the uploader the pipeline has stopped (whether success
        // or failure) so it runs one final drain pass and exits.
        // Ignore the send error: if the receiver is already gone, the
        // uploader task has already returned.
        let _ = stop_tx.send(());

        // Wait for the uploader to finish its final drain before we
        // return — otherwise the temp dir drops and we'd race the
        // final playlist upload against the directory disappearing.
        let uploader_result = uploader_handle
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("uploader join: {e}")))?;

        // Pipeline errors win: if the encode failed, surface that
        // regardless of what the uploader saw. Only if the pipeline
        // succeeded do we care about the uploader's outcome (it might
        // have failed to upload the final segment, for example).
        pipeline_result?;
        uploader_result?;

        // Temp dir is cleaned up when `temp_dir` drops here.
        Ok(())
    }
}

impl GstreamerTranscoder {
    /// Run the full GStreamer pipeline on a blocking thread. The pipeline
    /// is constructed and driven to EOS entirely synchronously because
    /// GStreamer's bus iteration blocks. Splitting this out of
    /// `transcode_to_hls` keeps the async method small enough to read
    /// at a glance.
    async fn run_pipeline_blocking(
        input_url: String,
        temp_path: std::path::PathBuf,
        quality_levels: Vec<QualityLevel>,
        has_audio: bool,
    ) -> Result<(), TranscoderError> {
        tokio::task::spawn_blocking(move || {
            Self::run_parallel_pipeline(&input_url, &temp_path, &quality_levels, has_audio)
        })
        .await
        .map_err(|e| TranscoderError::TranscodeFailed(format!("pipeline task join: {e}")))?
    }
}
