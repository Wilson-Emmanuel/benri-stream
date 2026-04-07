# Anonymous User Flows

> No accounts, no login. Upload a video, get a link, share it.

**BCs touched**: Video

---

## 1. Upload and Share

User has a product demo recording. They open benri-stream, drag in the file, give it a
title, and upload. After a short wait, the shareable link appears. They copy it and
share it.

---

## 2. Watching a Shared Video

Someone taps the link on their phone during a commute. Video starts playing right away.
Quality adjusts to their connection automatically — lower on mobile data, higher on wifi.
Standard controls — pause, seek, fullscreen, irrespective of the uploaded file size.

---

## 3. Damaged or Unusable File

User uploads a file that's damaged or that looks like a video but isn't decodable. The
system probes it before transcoding. If the probe fails, or if transcoding errors out
mid-stream, the video is marked failed and scheduled for deletion. No shareable link
is generated. The user sees an error after uploading.

Validation catches obviously bad files before upload (client-side type check, server-side
magic-byte signature check) to reduce wasted work.
