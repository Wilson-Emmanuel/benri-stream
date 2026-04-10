# Transcoding

Converts uploaded video files into adaptive-quality HLS streams. The transcoder reads from object storage via presigned URL, writes segments to a local temp directory, and a concurrent upload task pushes them to storage as they complete.

---

## Pipeline

One GStreamer pipeline per job. Input is decoded once, then a `tee` fans raw frames to N encoder branches (one per quality tier), each on its own thread via `queue`. Audio (if present) is decoded once, encoded once as AAC, and shared across all tiers.

```
uridecodebin3 ─┬─ videoconvert → video_tee ─┬─ queue → scale → x264enc → h264parse ─┐
               │                            ├─ queue → scale → x264enc → h264parse ─┤
               │                            └─ queue → scale → x264enc → h264parse ─┤
               │                                                                      ├──▶ mpegtsmux → hlssink2
               └─ audioconvert → audioresample → avenc_aac → aacparse → audio_tee ──┘
                  (only built if source has audio)
```

Properties:
- One decode, N encodes — input read and decoded once
- Audio encoded once, shared across tiers
- Encoder branches run on separate threads (wall-time bounded by slowest branch)
- Low quality finishes first, driving time-to-stream

---

## Early Publish

The `HlsUploader` polls the temp directory every 500ms and uploads completed segments to S3. When the low tier's first segment is durable in storage:

1. Synthesizes and uploads `master.m3u8` (deterministic from quality ladder + `has_audio` flag)
2. Force-uploads the current `low/playlist.m3u8`
3. Fires a one-shot `FirstSegmentNotifier`

The application layer uses the notification to write `share_token` on the video row, making the video watchable before the full transcode finishes.

Variant playlists are synthesized from the uploaded-segment list (hlssink2's on-disk playlist is ignored). On final drain, each variant playlist is closed with `#EXT-X-ENDLIST`.

---

## In-Memory Upload

Segments are read into `Vec<u8>` then uploaded via `ByteStream::from`, not `ByteStream::from_path`. The streaming path stalls ~30s per request against MinIO. Segments are a few MB each, so the buffered path is both faster (<15ms) and simpler.

---

## Port and Implementation

**Port** — `TranscoderPort` in `domain/src/ports/transcoder.rs`:

```
probe(storage_key) -> ProbeResult
transcode_to_hls(input_key, output_prefix, probe, notifier) -> ()
```

`probe()` validates the file and captures stream details (`has_audio`, dimensions). `transcode_to_hls` takes a `FirstSegmentNotifier` (wrapping a `oneshot::Sender`); fired once when master playlist + first segment are durable. If the transcoder errors first, the notifier is dropped unfired.

**Implementation** — `GstreamerTranscoder` in `infrastructure/src/transcoder/gstreamer.rs`. Accepts `Arc<dyn StoragePort>` and a `Vec<QualityLevel>` parsed from config.

---

## Quality Tiers

| Level | Resolution | Bitrate | Segment Duration |
|-------|-----------|---------|-----------------|
| Low | 640x360 | ~800 kbps | 4s |
| Medium | 1280x720 | ~2500 kbps | 4s |
| High | 1920x1080 | ~5000 kbps | 4s |

Configurable via `QUALITY_TIERS` env var (comma-separated, e.g. `low,medium`). Unknown entries are dropped; empty list falls back to the full ladder. Defined in `infrastructure/src/transcoder/quality.rs`.

Quality tiers are an infrastructure detail — the domain port says "transcode to HLS" without knowing levels or resolutions.

---

## GPU Acceleration

GStreamer auto-detects hardware encoders at runtime. Same pipeline code runs on GPU or falls back to CPU software encoding.
