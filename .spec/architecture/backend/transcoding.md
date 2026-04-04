# Transcoding

Converts uploaded video files into adaptive-quality HLS streams. The transcoder reads
from object storage and writes output directly to object storage — no local disk.

---

## What It Does

1. **Probe** — reads file headers from storage to confirm it's a valid, processable video
2. **Transcode** — decodes the input and encodes at three quality levels simultaneously,
   producing HLS segments and playlists
3. **Output** — writes segments and master manifest directly to object storage as they're
   produced. Video becomes streamable progressively.

---

## Pipeline Structure

```
Object Storage (input)
    │
    ▼
  Source ──▶ Decode ──┬──▶ Encode (low, 360p)  ──▶ HLS Mux ──▶ Storage Sink (low/)
                      ├──▶ Encode (medium, 720p) ──▶ HLS Mux ──▶ Storage Sink (medium/)
                      └──▶ Encode (high, 1080p)  ──▶ HLS Mux ──▶ Storage Sink (high/)

                      + Master manifest → Storage Sink (master.m3u8)
```

All three quality levels are produced per segment simultaneously. The player gets
adaptive bitrate from the first streamable segment.

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
        quality_levels: &[QualityLevel],
        on_first_segment: Box<dyn FnOnce() + Send>,
    ) -> Result<TranscodeResult, TranscoderError>;
}
```

The `on_first_segment` callback notifies the use case when the first segment is ready
(triggers share token generation). The trait knows nothing about how transcoding happens.

**Implementation** (infrastructure):
```rust
// crates/infrastructure/src/transcoder/gstreamer.rs
pub struct GstreamerTranscoder { storage: Arc<dyn StoragePort> }
impl TranscoderPort for GstreamerTranscoder { ... }
```

Uses `gstreamer-rs` to build the pipeline. The source element reads from a presigned
storage URL. Sink elements write segments to storage via the `StoragePort`. No local
filesystem involved.

---

## Quality Levels

| Level | Resolution | Target Bitrate | Segment Duration |
|-------|-----------|---------------|-----------------|
| Low | 640×360 | ~800 kbps | 6 seconds |
| Medium | 1280×720 | ~2500 kbps | 6 seconds |
| High | 1920×1080 | ~5000 kbps | 6 seconds |

Defined as an enum in the domain. The transcoder implementation reads these values —
adjusting quality levels is a domain change, not an infrastructure one.

---

## GPU Acceleration

GStreamer auto-detects available hardware encoders at runtime. Same pipeline code runs
on GPU or falls back to CPU software encoding — no code changes. The balance between
instance capability (GPU vs CPU) and worker count is a deployment decision.

---

## Configuration

| Config | Where | Description |
|--------|-------|-------------|
| Quality levels (resolution, bitrate) | `domain` | `src/video/quality.rs` — enum with resolution/bitrate methods |
| Segment duration | `infrastructure` | `src/transcoder/gstreamer.rs` — pipeline config |
| Codec settings (preset, profile) | `infrastructure` | `src/transcoder/gstreamer.rs` — pipeline config |

No environment variables for transcoding — quality levels are domain constants, codec
settings are implementation details in the infrastructure.

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| `TranscoderPort` trait | `domain` | `src/ports/transcoder.rs` |
| `TranscoderError`, `ProbeResult`, `TranscodeResult` | `domain` | `src/ports/transcoder.rs` |
| `QualityLevel` enum (resolution, bitrate) | `domain` | `src/video/quality.rs` |
| `GstreamerTranscoder` implementation | `infrastructure` | `src/transcoder/gstreamer.rs` |
| Wiring (construct transcoder, pass to use cases) | `worker` | `src/main.rs` |
