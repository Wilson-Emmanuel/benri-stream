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

## 3. Progressive Availability

User uploads a long lecture. The video becomes watchable from the beginning before the
whole thing is fully ready. People clicking the link can start watching right away.

---

## 4. Partially Broken File

User uploads a file that's damaged halfway through. The first half plays fine — the
video just ends earlier than expected. Viewers aren't shown an error. (See
[clarifications #8](../../clarifications.md) — I'm assuming keep-what-works, pending
confirmation.)

Validation catches obviously bad files before upload to reduce the chance of this.

---

## 5. Completely Unusable File

User uploads a file that looks like a video but isn't. The system can't do anything with
it. No shareable link is ever generated. The user sees an error after uploading.
