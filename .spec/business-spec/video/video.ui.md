# Video — Frontend Spec

> **Backend spec**: [video.md](video.md)
> **User stories**: [anonymous-user.md](../user-stories/anonymous-user.md)

## Changelog

| Date | Change | Author |
|------|--------|--------|
| 2026-04-04 | Initial spec | Wilson |

---

## Screens

### Upload Page {#SCR-VID-001}

**Route**: `/`
**Entry points**:
  - Direct visit
**Layout**: Centered single column. Drop zone, title input, upload button. After upload
completes, shows processing status and eventually the shareable link.
**Interactions**: UI-VID-001, UI-VID-002, UI-VID-003, UI-VID-004

---

### Player Page {#SCR-VID-002}

**Route**: `/v/{shareToken}`
**Entry points**:
  - Shareable link (from upload page, shared via chat/email/etc.)
**Layout**: Centered column. Video title above the player. Full-width video player below.
**Interactions**: UI-VID-005, UI-VID-006

---

## Interactions

### Select File {#UI-VID-001}

**Type**: Local
**Triggered by**: User drags a file onto the drop zone, or clicks to browse
**Screen**: [SCR-VID-001](#scr-vid-001)

**Behavior**
1. Validate file type against supported formats
2. Validate file size does not exceed 1 GB
3. If valid, show filename and size in the drop zone. Pre-fill title input from filename (without extension)
4. If invalid, show error message. Do not pre-fill title

**Visual Feedback**

| Condition | Display |
|-----------|---------|
| Dragging over drop zone | Highlight border |
| Valid file selected | Filename + size shown in drop zone |
| Invalid type | "Unsupported format" error |
| Invalid size | "File exceeds 1 GB" error |

---

### Upload Video {#UI-VID-002}

**Type**: Connected
**Use Case**: [UC-VID-001](video.md#uc-vid-001) → [UC-VID-002](video.md#uc-vid-002)
**Triggered by**: User clicks Upload button
**Screen**: [SCR-VID-001](#scr-vid-001)

**Form Fields**

| Field | Maps to UC Input | Widget | Client Validation |
|-------|-----------------|--------|-------------------|
| Title | `title` | Text input | Not blank, max 100 chars |
| File | (uploaded via presigned URL) | Drop zone | Type + size (see UI-VID-001) |

**States**

| State | Visual Behavior |
|-------|----------------|
| Uploading | Progress indicator. Upload button disabled |
| Completing | "Finalizing..." text. Button disabled |
| Error | Error message. Button re-enabled for retry |

**Error Display**

| Error Code | Display |
|------------|---------|
| `UNSUPPORTED_FORMAT` | "This file format is not supported" |
| `TITLE_REQUIRED` | "Please enter a title" |
| `TITLE_TOO_LONG` | "Title is too long" |
| `FILE_NOT_FOUND_IN_STORAGE` | "Upload failed — please try again" |
| `FILE_TOO_LARGE` | "File exceeds 1 GB" |
| `INVALID_FILE_SIGNATURE` | "This file doesn't appear to be a valid video" |

---

### Poll for Link {#UI-VID-003}

**Type**: Connected
**Use Case**: [UC-VID-003](video.md#uc-vid-003)
**Triggered by**: Automatically after upload completes (UI-VID-002 succeeds)
**Screen**: [SCR-VID-001](#scr-vid-001)

**Behavior**
1. Poll `GET /api/videos/{id}/status` every 5 seconds
2. While `shareUrl` is null and status is not `FAILED` → show "Processing..."
3. When `shareUrl` appears → stop polling, show the link (UI-VID-004)
4. If status is `FAILED` → stop polling, show "This video could not be processed"

**States**

| State | Visual Behavior |
|-------|----------------|
| Processing | "Processing..." with spinner |
| Ready | Shareable link displayed |
| Failed | Error message |

---

### Copy Shareable Link {#UI-VID-004}

**Type**: Local
**Triggered by**: User clicks Copy button next to the shareable link
**Screen**: [SCR-VID-001](#scr-vid-001)

**Behavior**
1. Copy the shareable URL to clipboard
2. Brief visual confirmation ("Copied!")

---

### Load and Play Video {#UI-VID-005}

**Type**: Connected
**Use Case**: [UC-VID-004](video.md#uc-vid-004)
**Triggered by**: Page load
**Screen**: [SCR-VID-002](#scr-vid-002)

**Behavior**
1. Fetch video metadata via `GET /api/videos/share/{shareToken}`
2. If `streamUrl` is present → initialize HLS player
3. If `streamUrl` is null → show title with "Processing..." and poll (UI-VID-006)
4. Quality switching handled automatically by the HLS player — no UI controls needed

**States**

| State | Visual Behavior |
|-------|----------------|
| Loading | Loading indicator |
| Playing | Video player with standard controls (pause, seek, fullscreen) |
| Not yet streamable | Title visible, "Processing..." with spinner |

**Error Display**

| Error Code | Display |
|------------|---------|
| `VIDEO_NOT_FOUND` | "This video doesn't exist" |

---

### Poll While Processing {#UI-VID-006}

**Type**: Connected
**Use Case**: [UC-VID-004](video.md#uc-vid-004)
**Triggered by**: Player page loads and `streamUrl` is null
**Screen**: [SCR-VID-002](#scr-vid-002)

**Behavior**
1. Poll `GET /api/videos/share/{shareToken}` every 3 seconds
2. When `streamUrl` appears → stop polling, initialize player (UI-VID-005)
