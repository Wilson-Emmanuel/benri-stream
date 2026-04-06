use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use domain::ports::storage::StoragePort;
use domain::ports::transcoder::{ProbeResult, TranscodeResult, TranscoderError, TranscoderPort};

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

    /// Build and run the HLS transcoding pipeline for a single quality level.
    /// Returns the number of segments produced.
    fn run_pipeline_for_level(
        input_url: &str,
        output_dir: &Path,
        level: &QualityLevel,
    ) -> Result<u32, TranscoderError> {
        use gstreamer as gst;
        use gstreamer_video as gst_video;

        let pipeline = gst::Pipeline::new();

        // Source: decode from URL
        let src = gst::ElementFactory::make("uridecodebin")
            .property("uri", input_url)
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("uridecodebin: {e}")))?;

        // Video processing: scale + encode
        let queue = gst::ElementFactory::make("queue").build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("queue: {e}")))?;
        let convert = gst::ElementFactory::make("videoconvert").build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("videoconvert: {e}")))?;
        let scale = gst::ElementFactory::make("videoscale").build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("videoscale: {e}")))?;

        let (width, height) = level.resolution();
        let capsfilter = gst::ElementFactory::make("capsfilter")
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
        let enc = gst::ElementFactory::make("x264enc")
            .property("bitrate", bitrate_kbps)
            .property_from_str("tune", "zerolatency")
            .property_from_str("speed-preset", "fast")
            .property("key-int-max", (SEGMENT_DURATION_SECS * 30) as u32) // keyframe every segment
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("x264enc: {e}")))?;

        let h264parse = gst::ElementFactory::make("h264parse").build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("h264parse: {e}")))?;

        // HLS mux: writes segments to local temp dir
        let level_dir = output_dir.join(level.name());
        std::fs::create_dir_all(&level_dir)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("mkdir: {e}")))?;

        let playlist_path = level_dir.join("playlist.m3u8");
        let segment_pattern = level_dir.join("segment_%05d.ts");

        let hlssink = gst::ElementFactory::make("hlssink3")
            .property("target-duration", SEGMENT_DURATION_SECS)
            .property("playlist-length", 0u32) // VOD — keep all segments
            .property("playlist-location", playlist_path.to_str().unwrap())
            .property("location", segment_pattern.to_str().unwrap())
            .build()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("hlssink3: {e}")))?;

        pipeline
            .add_many([&src, &queue, &convert, &scale, &capsfilter, &enc, &h264parse, &hlssink])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("add elements: {e}")))?;

        gst::Element::link_many([&queue, &convert, &scale, &capsfilter, &enc, &h264parse, &hlssink])
            .map_err(|e| TranscoderError::TranscodeFailed(format!("link elements: {e}")))?;

        // uridecodebin has dynamic pads — connect video pad when available
        let queue_weak = queue.downgrade();
        src.connect_pad_added(move |_, pad| {
            let Some(caps) = pad.current_caps() else { return };
            let Some(s) = caps.structure(0) else { return };
            if !s.name().starts_with("video/") {
                return;
            }
            let Some(queue) = queue_weak.upgrade() else { return };
            if let Some(sink_pad) = queue.static_pad("sink") {
                let _ = pad.link(&sink_pad);
            }
        });

        // Run the pipeline
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| TranscoderError::TranscodeFailed(format!("set playing: {e}")))?;

        let bus = pipeline.bus().unwrap();
        let mut error: Option<String> = None;

        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(err) => {
                    error = Some(format!(
                        "{} (debug: {:?})",
                        err.error(),
                        err.debug()
                    ));
                    break;
                }
                _ => {}
            }
        }

        pipeline.set_state(gst::State::Null).ok();

        if let Some(err_msg) = error {
            return Err(TranscoderError::TranscodeFailed(err_msg));
        }

        // Count produced segments
        let segment_count = std::fs::read_dir(&level_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "ts")
                            .unwrap_or(false)
                    })
                    .count() as u32
            })
            .unwrap_or(0);

        Ok(segment_count)
    }
}

#[async_trait]
impl TranscoderPort for GstreamerTranscoder {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError> {
        use gstreamer as gst;
        use gstreamer_pbutils as gst_pbutils;

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

            Ok(ProbeResult {
                duration_seconds,
                width,
                height,
                codec,
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
        on_first_segment: Box<dyn FnOnce() + Send>,
    ) -> Result<TranscodeResult, TranscoderError> {
        use gstreamer as gst;

        tracing::info!(input_key, output_prefix, "transcoder: starting HLS transcode");

        gst::init().map_err(|e| TranscoderError::TranscodeFailed(format!("gst init: {e}")))?;

        let input_url = self.input_url(input_key).await?;
        let storage = self.storage.clone();
        let output_prefix = output_prefix.to_string();

        // Create temp directory for this job
        let temp_dir = tempfile::tempdir()
            .map_err(|e| TranscoderError::TranscodeFailed(format!("tempdir: {e}")))?;
        let temp_path = temp_dir.path().to_path_buf();

        let quality_levels = QualityLevel::all().to_vec();
        let mut total_segments: u32 = 0;
        let mut first_segment_notified = false;

        // Process each quality level. The GStreamer pipeline runs synchronously
        // (blocking on the bus), so we run each in spawn_blocking.
        // All three levels decode from the same URL independently.
        // This is parallel at the quality level — each level runs its own pipeline.
        for level in &quality_levels {
            let url = input_url.clone();
            let dir = temp_path.clone();
            let lvl = *level;

            let segment_count = tokio::task::spawn_blocking(move || {
                Self::run_pipeline_for_level(&url, &dir, &lvl)
            })
            .await
            .map_err(|e| TranscoderError::TranscodeFailed(format!("task join: {e}")))??;

            if segment_count > 0 {
                // Upload all segments and playlist for this level
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

                total_segments += segment_count;

                // Notify on first successful segment
                if !first_segment_notified {
                    first_segment_notified = true;
                    on_first_segment();
                }
            }
        }

        // Generate and upload master playlist
        if total_segments > 0 {
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
        }

        // Temp dir is cleaned up on drop
        Ok(TranscodeResult {
            segments_produced: total_segments,
        })
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
