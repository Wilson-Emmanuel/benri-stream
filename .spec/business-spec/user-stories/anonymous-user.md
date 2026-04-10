# Anonymous User Flows

> No accounts, no login. Upload a video, get a link, share it.

**BCs touched**: Video

---

## 1. Upload and Share

The user opens benri-stream, drags in a video file, gives it a title, and uploads. After a short wait, a shareable link appears. They copy it and send it.

---

## 2. Watching a Shared Video

Someone opens the link. The video starts playing immediately with quality adapting to their connection. Standard controls: pause, seek, fullscreen.

---

## 3. Damaged or Unusable File

The user uploads a file that is damaged or not decodable. The system probes it before transcoding; if the probe or transcode fails, the video is marked failed and scheduled for deletion. No shareable link is generated. Client-side type checks and server-side magic-byte validation catch obviously bad files before work begins.
