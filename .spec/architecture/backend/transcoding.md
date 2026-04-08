# Transcoding

Converts uploaded video files into adaptive-quality HLS streams. The transcoder reads
from object storage via presigned URL. Segments are written to a local temp directory,
uploaded to object storage as they complete, and deleted. Nothing persists between jobs.

---

## What It Does

1. **Probe** — reads file headers from storage to confirm it's a valid, processable video
2. **Transcode** — decodes the input and encodes at three quality levels simultaneously,
   producing HLS segments and playlists
3. **Progressive output** — a background uploader task, running concurrently with the
   GStreamer pipeline, uploads segments and variant playlists to object storage as
   hlssink2 produces them (not in a single batch at the end). Video becomes streamable
   before the full transcode finishes.
4. **First-segment trigger** — the moment the low tier's first segment is durably
   uploaded, the uploader also generates `master.m3u8` (content is deterministic from
   the quality ladder and the probe's `has_audio` flag, so no coordination with the
   encoder is needed), force-uploads the current `low/playlist.m3u8` alongside it,
   and fires a one-shot `FirstSegmentNotifier`. The application layer uses that
   notification to write the share token to the video row (see
   [video.md#uc-vid-005](../../business-spec/video/video.md#uc-vid-005)).

---

## Pipeline Structure

One GStreamer pipeline per transcode job. The input is decoded once, then a `tee`
element fans the raw frames to N encoder branches — one per quality level — each
scheduled on its own thread by the `queue` element right after the tee. Audio, if
present, is decoded once, encoded once as AAC, and shared across all levels via a
second tee (audio bitrate doesn't change across tiers, so re-encoding per tier would
be wasted work).

```
uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale(360p)  → x264enc → h264parse ─┐
               │                            ├─ queue → scale(720p)  → x264enc → h264parse ─┤
               │                            └─ queue → scale(1080p) → x264enc → h264parse ─┤
               │                                                                            ├──▶ mpegtsmux(per level) ──▶ hlssink3(per level)
               └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ─────────┘
                  (only built if the source has an audio stream)
```

**Why each element**:
- **`uridecodebin3`** — modern streams-aware source. Reads from the presigned storage
  URL, auto-detects demuxer + decoder. Stable since GStreamer 1.22. Preferred over the
  older `uridecodebin` for more accurate HTTP buffering (less over-download of large
  source files from S3) and cleaner multi-stream handling.
- **`videoconvert`** — normalizes decoded frames to a common format before the tee, so
  all branches see consistent input
- **`video_tee`** — fans the decoded video stream to N branches. Src pads are requested
  dynamically (`src_%u`), one per level
- **`queue`** (after each tee src pad) — critical for parallelism. GStreamer puts each
  side of a queue on its own streaming thread, so the encoder branches actually run
  concurrently
- **`videoscale` + `capsfilter`** — scales to the target resolution per branch
- **`x264enc`** — H.264 encoder at the per-level target bitrate
- **`h264parse`** — parses the encoded stream into frames `mpegtsmux` can mux
- **`audioconvert` + `audioresample`** — normalizes decoded audio to something the
  encoder will accept
- **`avenc_aac`** — AAC audio encoder at 128 kbps (shared across all quality tiers)
- **`aacparse` + `audio_tee`** — parses and fans the encoded AAC out to N muxers
- **`mpegtsmux`** (one per level) — combines this level's video with the shared audio
  into an MPEG-TS stream
- **`hlssink3`** (one per level) — writes .ts segments and per-level `playlist.m3u8`
  to a local temp dir

Segments and variant playlists are uploaded to object storage as the pipeline
produces them, by a separate tokio task (`HlsUploader`) that polls the
per-tier output directories every 500 ms. The master playlist is generated
and uploaded the moment the low tier's first segment becomes durable — not
at the end of the pipeline — and that same moment fires the one-shot
first-segment notifier the application layer uses to publish the share
link. See `crates/infrastructure/src/transcoder/hls_uploader.rs`.

**Audio handling**: a quick `Discoverer` call before building the pipeline tells us
whether the source has an audio stream. If yes, we build the audio branch and wire it
into every level's `mpegtsmux`. If no, we skip the audio branch entirely — `mpegtsmux`
is fine with video-only input.

**Parallelism properties**:
- **One decode, N encodes** — the input is read from storage and decoded exactly once
  (video and audio each once)
- **Audio encoded once** — not re-encoded per quality tier
- **Wall-time parallel** — encoders run on separate threads, so total time is bounded
  by the slowest branch (high/1080p), not the sum
- **Low quality finishes first** — naturally, because low resolution means less
  encoding work per frame — which is what drives time-to-stream

---

## Port and Implementation

**Port trait** (domain):
```rust
// crates/domain/src/ports/transcoder.rs
pub trait TranscoderPort: Send + Sync {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, TranscoderError>;
    async fn transcode_to_hls(
        &self,
        input_key: &str,
        output_prefix: &str,
        probe: &ProbeResult,
        first_segment_ready: Box<dyn FirstSegmentNotifier>,
    ) -> Result<(), TranscoderError>;
}

pub trait FirstSegmentNotifier: Send + Sync {
    fn notify(self: Box<Self>);
}
```

`probe()` validates the file is decodable AND captures stream-level details
(`has_audio`, dimensions, codec) so `transcode_to_hls` doesn't have to re-read
headers from S3.

`transcode_to_hls` takes a `FirstSegmentNotifier` the application layer
constructs (typically wrapping a `tokio::sync::oneshot::Sender`). The
transcoder fires it exactly once, the moment the master playlist and the
low tier's first segment are both durable in storage — this is the
earliest point at which a viewer holding a share link could begin playback.
If the transcoder errors before that moment, the notifier is dropped
without being called; the caller treats "notifier never fired" as a normal
failure outcome.

The trait knows nothing about how transcoding happens — quality levels,
codecs, and segment config are internal to the infrastructure
implementation. Transcode either runs to completion or fails wholesale;
there is no partial-success preservation path (failures still mark the
video `FAILED` and schedule `DeleteVideo` regardless of how much output
made it to storage).

**Implementation** (infrastructure):
```rust
// crates/infrastructure/src/transcoder/gstreamer.rs
pub struct GstreamerTranscoder { storage: Arc<dyn StoragePort> }
impl TranscoderPort for GstreamerTranscoder { ... }
```

Uses `gstreamer-rs` to build the pipeline. The source element reads from a presigned
storage URL. `hlssink3` writes segments to a local temp directory. A signal handler
uploads each completed segment to storage via `StoragePort` and deletes the local file.
Temp directory is cleaned up when the job finishes.

---

## Quality Levels

| Level | Resolution | Target Bitrate | Segment Duration |
|-------|-----------|---------------|-----------------|
| Low | 640×360 | ~800 kbps | 4 seconds |
| Medium | 1280×720 | ~2500 kbps | 4 seconds |
| High | 1920×1080 | ~5000 kbps | 4 seconds |

Defined as an enum in infrastructure alongside the transcoder implementation. Quality
levels are an implementation detail — the domain port just says "transcode to HLS"
without knowing how many levels or what resolutions are produced.

---

## GPU Acceleration

GStreamer auto-detects available hardware encoders at runtime. Same pipeline code runs
on GPU or falls back to CPU software encoding — no code changes. The balance between
instance capability (GPU vs CPU) and worker count is a deployment decision.

---

## Configuration

| Config | Where | Description |
|--------|-------|-------------|
| Quality levels (resolution, bitrate) | `infrastructure` | `src/transcoder/quality.rs` — enum with resolution/bitrate methods |
| Segment duration | `infrastructure` | `src/transcoder/gstreamer.rs` — pipeline config |
| Codec settings (preset, profile) | `infrastructure` | `src/transcoder/gstreamer.rs` — pipeline config |

No environment variables for transcoding — quality levels are domain constants, codec
settings are implementation details in the infrastructure.

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| `TranscoderPort` trait | `domain` | `src/ports/transcoder.rs` |
| `TranscoderError`, `ProbeResult` | `domain` | `src/ports/transcoder.rs` |
| `QualityLevel` enum (resolution, bitrate) | `infrastructure` | `src/transcoder/quality.rs` |
| `GstreamerTranscoder` implementation | `infrastructure` | `src/transcoder/gstreamer.rs` |
| Wiring (construct transcoder, pass to use cases) | `worker` | `src/main.rs` |
