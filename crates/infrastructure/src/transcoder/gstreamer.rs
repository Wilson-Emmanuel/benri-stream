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

/// Target HLS segment duration. Visible to `hls_uploader` so synthesized
/// playlists declare matching `EXT-X-TARGETDURATION` and `EXTINF` values.
pub(super) const SEGMENT_DURATION_SECS: u32 = 4;

/// Presigned-GET TTL for the input file. Sized to outlast the maximum task
/// `processing_timeout` (30 min) with plenty of headroom.
const INPUT_PRESIGN_TTL_SECS: u64 = 2 * 60 * 60;

/// GStreamer-based transcoder. Reads from S3 via presigned URL, writes HLS
/// segments to a local temp dir, and uploads them concurrently via
/// [`HlsUploader`] while the pipeline is running. Worker instances are
/// stateless. `quality_tiers` order is preserved into the master playlist.
pub struct GstreamerTranscoder {
    storage: Arc<dyn StoragePort>,
    quality_tiers: Vec<QualityLevel>,
}

impl GstreamerTranscoder {
    pub fn new(storage: Arc<dyn StoragePort>, quality_tiers: Vec<QualityLevel>) -> Self {
        Self { storage, quality_tiers }
    }

    /// Presigned GET URL for the input file. Keeps `uploads/` private —
    /// only a worker with a fresh presign can read the original.
    async fn input_url(&self, storage_key: &str) -> Result<String, TranscoderError> {
        self.storage
            .generate_presigned_download_url(storage_key, INPUT_PRESIGN_TTL_SECS)
            .await
            .map_err(|e| {
                TranscoderError::TranscodeFailed(format!("presign download url: {e}"))
            })
    }

    /// Build the HLS pipeline. Input is decoded once and fanned out to one
    /// encoder branch per quality level. Audio (if present) is encoded once
    /// as AAC and shared across all tiers.
    ///
    /// ```text
    /// uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale(360p)  → x264enc → h264parse ─┐
    ///                │                            ├─ queue → scale(720p)  → x264enc → h264parse ─┤
    ///                │                            └─ queue → scale(1080p) → x264enc → h264parse ─┤
    ///                │                                                                            ├──▶ hlssink2(per level)
    ///                └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ────────┘
    ///                   (only when source has audio)
    /// ```
    ///
    /// A `queue` after each tee src pad gives each encoder branch its own
    /// streaming thread. Returns a linked pipeline ready for
    /// [`Self::drive_pipeline_to_eos`]. The build/drive split lets the caller
    /// hold the pipeline handle for out-of-band cancellation (spawn_blocking
    /// tasks aren't cancellable via their JoinHandle).
    fn build_parallel_pipeline(
        input_url: &str,
        output_dir: &Path,
        quality_levels: &[QualityLevel],
        has_audio: bool,
    ) -> Result<gstreamer::Pipeline, TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;

        let pipeline = gst::Pipeline::new();

        // uridecodebin3 is the streams-aware successor to uridecodebin,
        // with more accurate HTTP buffering — relevant when reading from
        // presigned S3 URLs where over-downloading incurs egress costs.
        let src = gst::ElementFactory::make("uridecodebin3")
            .property("uri", input_url)
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("uridecodebin3: {e}")))?;

        let (video_convert, video_tee) = Self::build_video_frontend(&pipeline)?;

        // ---- Audio front-end (only if source has audio) ----
        // Encoded once and shared across all tiers via audio_tee.
        let audio_chain = if has_audio {
            Some(Self::build_audio_frontend(&pipeline)?)
        } else {
            None
        };

        // Dynamic pad linking: uridecodebin3 exposes pads after stream
        // discovery. Route by pad name (`video_%u` / `audio_%u`) rather
        // than caps — caps are not yet negotiated when `pad-added` fires,
        // but pad names are part of the element template and always set.
        let video_convert_weak = video_convert.downgrade();
        let audio_convert_weak = audio_chain.as_ref().map(|(ac, _)| ac.downgrade());
        src.connect_pad_added(move |_, pad| {
            let pad_name = pad.name();
            if pad_name.starts_with("video_") {
                Self::link_dynamic_pad(&pad, &video_convert_weak, "sink", "video pad to videoconvert");
            } else if pad_name.starts_with("audio_") {
                if let Some(weak) = &audio_convert_weak {
                    Self::link_dynamic_pad(&pad, weak, "sink", "audio pad to audioconvert");
                }
                // If no audio chain was built: unusual, but possible when
                // streams differ between Discoverer and uridecodebin3.
            }
        });

        pipeline
            .add(&src)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add src: {e}")))?;

        // Build one encoder branch per quality level.
        for level in quality_levels {
            let level_dir = output_dir.join(level.name());
            std::fs::create_dir_all(&level_dir)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("mkdir: {e}")))?;

            let hlssink = Self::build_level_branch(&pipeline, &video_tee, level, &level_dir)?;

            if let Some((_, audio_tee_elem)) = &audio_chain {
                Self::link_audio_branch(&pipeline, audio_tee_elem, &hlssink)?;
            }
        }

        Ok(pipeline)
    }

    /// Add videoconvert and video tee to the pipeline and link them.
    fn build_video_frontend(
        pipeline: &gstreamer::Pipeline,
    ) -> Result<(gstreamer::Element, gstreamer::Element), TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;

        let video_convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("videoconvert: {e}")))?;
        let video_tee = gst::ElementFactory::make("tee")
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("video tee: {e}")))?;

        pipeline
            .add_many([&video_convert, &video_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add video front-end: {e}")))?;
        gst::Element::link_many([&video_convert, &video_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("link video front-end: {e}")))?;

        Ok((video_convert, video_tee))
    }

    /// Add the shared audio chain (audioconvert → audioresample → avenc_aac →
    /// aacparse → audio_tee) to the pipeline and link it. Returns
    /// (audio_convert, audio_tee) — the two elements needed for dynamic pad
    /// linking and per-level branch wiring.
    fn build_audio_frontend(
        pipeline: &gstreamer::Pipeline,
    ) -> Result<(gstreamer::Element, gstreamer::Element), TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;

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
            .add_many([&audio_convert, &audio_resample, &audio_enc, &audio_parse, &audio_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add audio front-end: {e}")))?;
        gst::Element::link_many([&audio_convert, &audio_resample, &audio_enc, &audio_parse, &audio_tee])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("link audio front-end: {e}")))?;

        Ok((audio_convert, audio_tee))
    }

    /// Attempt to link a dynamically-added source pad to the named sink pad of
    /// `target`. Skips silently if the sink is already linked (duplicate stream).
    fn link_dynamic_pad(
        pad: &gstreamer::Pad,
        target_weak: &gstreamer::glib::WeakRef<gstreamer::Element>,
        sink_pad_name: &str,
        label: &str,
    ) {
        use gstreamer::prelude::*;
        let Some(target) = target_weak.upgrade() else { return };
        let Some(sink_pad) = target.static_pad(sink_pad_name) else { return };
        if sink_pad.is_linked() {
            return;
        }
        if let Err(e) = pad.link(&sink_pad) {
            tracing::error!(error = %e, "failed to link {label}");
        }
    }

    /// Build the video encoder branch for one quality level, add it to the
    /// pipeline, wire video_tee → vqueue, and return the hlssink2 element so
    /// the caller can attach an audio branch if needed.
    fn build_level_branch(
        pipeline: &gstreamer::Pipeline,
        video_tee: &gstreamer::Element,
        level: &QualityLevel,
        level_dir: &Path,
    ) -> Result<gstreamer::Element, TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        use gstreamer_video as gst_video;

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
        // `ultrafast` trades compression ratio for encode speed (~3-5×
        // faster than `fast`). Wall-time is the constraint when three
        // tiers run in parallel on CPU. Bitrate ceilings in quality.rs
        // bound output size regardless of preset.
        let venc = gst::ElementFactory::make("x264enc")
            .property("bitrate", bitrate_kbps)
            .property_from_str("tune", "zerolatency")
            .property_from_str("speed-preset", "ultrafast")
            .property("key-int-max", SEGMENT_DURATION_SECS * 30)
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("x264enc: {e}")))?;

        // Pin H.264 to high profile, level 4.0 across all tiers so the
        // master playlist can carry a stable CODECS= attribute and the
        // player can pick a variant without fetching per-tier playlists.
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

        let hlssink = Self::make_hlssink(level_dir)?;

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

        Ok(hlssink)
    }

    /// Create and configure the hlssink2 element for a quality level directory.
    ///
    /// `max-files=0` disables hlssink2's segment rotation. The default (10)
    /// causes the concurrent uploader to occasionally read a segment that
    /// hlssink2 just deleted. We delete each segment ourselves once it's safely
    /// in S3, so disk usage stays bounded.
    fn make_hlssink(level_dir: &Path) -> Result<gstreamer::Element, TranscoderError> {
        use gstreamer as gst;

        let playlist_path = level_dir.join("playlist.m3u8");
        let segment_pattern = level_dir.join("segment_%05d.ts");
        gst::ElementFactory::make("hlssink2")
            .property("target-duration", SEGMENT_DURATION_SECS)
            .property("playlist-length", 0u32)
            .property("max-files", 0u32)
            .property("playlist-location", playlist_path.to_str().unwrap())
            .property("location", segment_pattern.to_str().unwrap())
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("hlssink2: {e}")))
    }

    /// Wire audio_tee → aqueue → hlssink2.audio for one quality level.
    fn link_audio_branch(
        pipeline: &gstreamer::Pipeline,
        audio_tee: &gstreamer::Element,
        hlssink: &gstreamer::Element,
    ) -> Result<(), TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;

        let aqueue = gst::ElementFactory::make("queue")
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("aqueue: {e}")))?;
        pipeline
            .add(&aqueue)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add aqueue: {e}")))?;

        // audio_tee → aqueue
        let audio_tee_src = audio_tee
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

        Ok(())
    }

    /// Drive a built pipeline to EOS or error, polling the bus in 500 ms
    /// intervals. Returns `Ok(())` on EOS, `Err` on pipeline error or when
    /// `cancel` is set. Always resets the pipeline to `Null` before returning.
    fn drive_pipeline_to_eos(
        pipeline: &gstreamer::Pipeline,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), TranscoderError> {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        use std::sync::atomic::Ordering;

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("set playing: {e}")))?;

        let bus = pipeline.bus().expect("pipeline has bus");
        let poll_timeout = gst::ClockTime::from_mseconds(500);
        let mut outcome: Result<(), TranscoderError> = Ok(());

        loop {
            if cancel.load(Ordering::Relaxed) {
                tracing::info!("pipeline cancellation requested; stopping bus loop");
                outcome = Err(TranscoderError::TranscodeFailed(
                    "cancelled by uploader failure".into(),
                ));
                break;
            }
            let msg = match bus.timed_pop(Some(poll_timeout)) {
                Some(m) => m,
                None => continue,
            };
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(err) => {
                    outcome = Err(TranscoderError::TranscodeFailed(format!(
                        "{} (debug: {:?})",
                        err.error(),
                        err.debug()
                    )));
                    break;
                }
                _ => {}
            }
        }

        pipeline.set_state(gst::State::Null).ok();
        outcome
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

        // Discoverer is synchronous — run on a blocking thread.
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
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

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
        let quality_levels = self.quality_tiers.clone();

        // Build synchronously: element construction is cheap and non-blocking,
        // and building here gives a pipeline handle we can share with both the
        // blocking driver and the cancellation path.
        let pipeline = Self::build_parallel_pipeline(
            &input_url,
            &temp_path,
            &quality_levels,
            probe.has_audio,
        )?;

        // Set to true by the uploader-failure path; the pipeline driver
        // checks it between bus polls and exits within 500 ms.
        let cancel = Arc::new(AtomicBool::new(false));

        // Start the uploader before the pipeline so it catches segments
        // from the first write.
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let uploader = HlsUploader::new(
            temp_path.clone(),
            self.storage.clone(),
            output_prefix.to_string(),
            quality_levels.clone(),
            probe.has_audio,
            first_segment_ready,
        );
        let mut uploader_handle = tokio::spawn(uploader.run(stop_rx));

        // Drive the pipeline on a blocking thread. The outer clone stays
        // on the async task for out-of-band cancellation.
        let mut pipeline_handle = tokio::task::spawn_blocking({
            let pipeline = pipeline.clone();
            let cancel = cancel.clone();
            move || Self::drive_pipeline_to_eos(&pipeline, &cancel)
        });

        // Race pipeline and uploader. Pipeline finishing first is the normal
        // case; uploader finishing first means it hit an error, so cancel
        // the pipeline and surface the uploader's error.
        let final_result: Result<(), TranscoderError> = tokio::select! {
            pipeline_join = &mut pipeline_handle => {
                let pipeline_result = pipeline_join.map_err(|e| {
                    TranscoderError::TranscodeFailed(format!("pipeline task join: {e}"))
                })?;
                // Signal the uploader to drain. No-op if it already exited.
                let _ = stop_tx.send(());
                let uploader_result = uploader_handle.await.map_err(|e| {
                    TranscoderError::TranscodeFailed(format!("uploader task join: {e}"))
                })?;
                pipeline_result.and(uploader_result)
            }
            uploader_join = &mut uploader_handle => {
                // Uploader exited early — must be an error. Cancel the
                // pipeline so it doesn't continue encoding into a dead
                // temp dir. Ignore the pipeline result; the uploader error
                // is the real cause.
                tracing::warn!(
                    "uploader exited before pipeline; cancelling pipeline to unwind transcode",
                );
                cancel.store(true, Ordering::Relaxed);
                let _ = pipeline_handle.await;
                match uploader_join {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(TranscoderError::TranscodeFailed(format!(
                        "uploader task join: {e}"
                    ))),
                }
            }
        };

        // Belt-and-braces: ensure Null state before temp_dir drops.
        use gstreamer::prelude::*;
        let _ = pipeline.set_state(gst::State::Null);

        final_result
    }
}
