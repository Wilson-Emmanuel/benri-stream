use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{ProbeResult, TranscoderError, TranscoderPort};

use super::quality::QualityLevel;

/// Target HLS segment duration in seconds. Shorter = faster time-to-stream,
/// longer = fewer files and better CDN cache efficiency. 4s is the balance.
const SEGMENT_DURATION_SECS: u32 = 4;

/// GStreamer-based transcoder. Reads from S3 via presigned URL, writes HLS
/// segments to a local temp dir, uploads each to S3 as it completes, then
/// deletes the local file.
/// Workers are stateless — nothing persists between jobs.
pub struct GstreamerTranscoder {
    storage: Arc<dyn StoragePort>,
}

impl GstreamerTranscoder {
    pub fn new(storage: Arc<dyn StoragePort>) -> Self {
        Self { storage }
    }

    /// Generate a presigned URL for reading the input file from storage.
    async fn input_url(&self, storage_key: &str) -> Result<String, TranscoderError> {
        let url = self.storage.public_url(storage_key);
        Ok(url)
    }

    /// Upload a local file to storage, then delete the local copy.
    async fn upload_and_delete(
        storage: &dyn StoragePort,
        local_path: &Path,
        storage_key: &str,
        content_type: &str,
    ) -> Result<(), TranscoderError> {
        storage
            .upload_from_path(local_path, storage_key, content_type)
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("upload failed: {e}")))?;
        let _ = tokio::fs::remove_file(local_path).await;
        Ok(())
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
    ///                │                                                                            ├──▶ mpegtsmux(per level) ──▶ hlssink3(per level)
    ///                └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ────────┘
    ///                   (only built if the source has an audio stream)
    /// ```
    ///
    /// The `queue` element after each tee src pad puts the branch on its own
    /// streaming thread — that's how the encoders actually run in parallel.
    ///
    /// `has_audio` is determined upstream by `probe()` and threaded in,
    /// so we don't have to re-read the file headers from S3.
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

        // Source: uridecodebin3 is the modern, streams-aware decode element.
        // Stable since GStreamer 1.22; we're on 1.28. It has more accurate
        // HTTP buffering than the older uridecodebin — relevant when reading
        // from presigned S3 URLs where over-downloading costs real egress.
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
        let video_convert_weak = video_convert.downgrade();
        let audio_convert_weak = audio_chain.as_ref().map(|(ac, _)| ac.downgrade());
        src.connect_pad_added(move |_, pad| {
            let Some(caps) = pad.current_caps() else {
                tracing::warn!("uridecodebin3: pad-added without caps");
                return;
            };
            let Some(structure) = caps.structure(0) else {
                tracing::warn!("uridecodebin3: pad-added caps without structure");
                return;
            };
            let name = structure.name();

            if name.starts_with("video/") {
                let Some(convert) = video_convert_weak.upgrade() else { return };
                let Some(sink_pad) = convert.static_pad("sink") else { return };
                if sink_pad.is_linked() {
                    // Already linked — ignore additional video streams.
                    return;
                }
                if let Err(e) = pad.link(&sink_pad) {
                    tracing::error!(error = %e, "failed to link video pad to videoconvert");
                }
            } else if name.starts_with("audio/") {
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
            let venc = gst::ElementFactory::make("x264enc")
                .property("bitrate", bitrate_kbps)
                .property_from_str("tune", "zerolatency")
                .property_from_str("speed-preset", "fast")
                .property("key-int-max", (SEGMENT_DURATION_SECS * 30) as u32)
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("x264enc: {e}")))?;

            let vparse = gst::ElementFactory::make("h264parse")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("h264parse: {e}")))?;

            // ---- Per-level MPEG-TS mux + HLS sink ----
            let mux = gst::ElementFactory::make("mpegtsmux")
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("mpegtsmux: {e}")))?;

            let playlist_path = level_dir.join("playlist.m3u8");
            let segment_pattern = level_dir.join("segment_%05d.ts");
            let hlssink = gst::ElementFactory::make("hlssink3")
                .property("target-duration", SEGMENT_DURATION_SECS)
                .property("playlist-length", 0u32)
                .property("playlist-location", playlist_path.to_str().unwrap())
                .property("location", segment_pattern.to_str().unwrap())
                .build()
                .map_err(|e| TranscoderError::TranscodeFailed(format!("hlssink3: {e}")))?;

            pipeline
                .add_many([&vqueue, &vscale, &vcaps, &venc, &vparse, &mux, &hlssink])
                .map_err(|e| TranscoderError::TranscodeFailed(format!("add level branch: {e}")))?;

            gst::Element::link_many([&vqueue, &vscale, &vcaps, &venc, &vparse])
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link video chain: {e}")))?;

            // vparse → mpegtsmux (request pad)
            let mux_video_sink = mux
                .request_pad_simple("sink_%d")
                .ok_or_else(|| TranscoderError::TranscodeFailed("mux video sink pad".into()))?;
            let vparse_src = vparse
                .static_pad("src")
                .ok_or_else(|| TranscoderError::TranscodeFailed("vparse src pad".into()))?;
            vparse_src
                .link(&mux_video_sink)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link vparse→mux: {e}")))?;

            // mpegtsmux → hlssink3
            mux.link(&hlssink)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("link mux→hlssink: {e}")))?;

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

                // aqueue → mpegtsmux (second request pad)
                let mux_audio_sink = mux
                    .request_pad_simple("sink_%d")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("mux audio sink pad".into()))?;
                let aqueue_src = aqueue
                    .static_pad("src")
                    .ok_or_else(|| TranscoderError::TranscodeFailed("aqueue src pad".into()))?;
                aqueue_src
                    .link(&mux_audio_sink)
                    .map_err(|e| TranscoderError::TranscodeFailed(format!("link aqueue→mux: {e}")))?;
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
        let result = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| TranscoderError::ProbeFailed(format!("task join: {e}")))?;

        result
    }

    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        probe: &ProbeResult,
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
        let storage = self.storage.clone();
        let output_prefix = output_prefix.to_string();
        let has_audio = probe.has_audio;

        // Create temp directory for this job
        let temp_dir = tempfile::tempdir()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("tempdir: {e}")))?;
        let temp_path = temp_dir.path().to_path_buf();

        let quality_levels = QualityLevel::all().to_vec();

        // Run one GStreamer pipeline that decodes the input once and fans
        // out to N encoder branches in parallel. GStreamer blocks on the
        // bus, so we run the whole thing in spawn_blocking.
        {
            let url = input_url.clone();
            let dir = temp_path.clone();
            let levels = quality_levels.clone();
            tokio::task::spawn_blocking(move || {
                Self::run_parallel_pipeline(&url, &dir, &levels, has_audio)
            })
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("task join: {e}")))??;
        }

        // Upload all per-level outputs (segments + per-level playlist).
        // The pipeline ran to EOS without error, so each level dir has
        // its full output.
        for level in &quality_levels {
            let level_dir = temp_path.join(level.name());
            let mut entries: Vec<_> = std::fs::read_dir(&level_dir)
                .map_err(|e| TranscoderError::TranscodeFailed(format!("readdir: {e}")))?
                .filter_map(|e| e.ok())
                .collect();
            entries.sort_by_key(|e| e.file_name());

            for entry in &entries {
                let path = entry.path();
                let filename = path.file_name().unwrap().to_str().unwrap();
                let s3_key = format!("{}{}/{}", output_prefix, level.name(), filename);
                let content_type = if filename.ends_with(".m3u8") {
                    "application/vnd.apple.mpegurl"
                } else {
                    "video/mp2t"
                };

                Self::upload_and_delete(storage.as_ref(), &path, &s3_key, content_type).await?;
            }
        }

        // Generate and upload master playlist
        let master_content = Self::generate_master_playlist(&quality_levels);
        let master_path = temp_path.join("master.m3u8");
        std::fs::write(&master_path, &master_content)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("write master: {e}")))?;

        let master_key = format!("{}master.m3u8", output_prefix);
        Self::upload_and_delete(
            storage.as_ref(),
            &master_path,
            &master_key,
            "application/vnd.apple.mpegurl",
        )
        .await?;

        // Temp dir is cleaned up on drop
        Ok(())
    }
}

impl GstreamerTranscoder {
    fn generate_master_playlist(levels: &[QualityLevel]) -> String {
        let mut m3u8 = String::from("#EXTM3U\n");
        for level in levels {
            let (width, height) = level.resolution();
            let bitrate = level.target_bitrate_bps();
            m3u8.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{}\n{}/playlist.m3u8\n",
                bitrate, width, height, level.name()
            ));
        }
        m3u8
    }
}
