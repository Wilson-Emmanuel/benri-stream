const API_BASE = '/api';

export async function initiateUpload(title, mimeType, filename) {
	const res = await fetch(`${API_BASE}/videos/initiate`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ title, mime_type: mimeType, filename })
	});
	if (!res.ok) {
		const err = await res.json();
		throw new Error(err.error || 'Failed to initiate upload');
	}
	return res.json();
}

export async function uploadToStorage(uploadUrl, file, headers) {
	const reqHeaders = {};
	for (const h of headers) {
		reqHeaders[h.name] = h.value;
	}
	const res = await fetch(uploadUrl, {
		method: 'PUT',
		headers: reqHeaders,
		body: file
	});
	if (!res.ok) {
		throw new Error('Upload to storage failed');
	}
}

export async function completeUpload(videoId) {
	const res = await fetch(`${API_BASE}/videos/${videoId}/complete`, {
		method: 'POST'
	});
	if (!res.ok) {
		const err = await res.json();
		throw new Error(err.error || 'Failed to complete upload');
	}
	return res.json();
}

export async function getVideoStatus(videoId) {
	const res = await fetch(`${API_BASE}/videos/${videoId}/status`);
	if (!res.ok) {
		const err = await res.json();
		throw new Error(err.error || 'Failed to get status');
	}
	return res.json();
}

export async function getVideoByToken(shareToken) {
	const res = await fetch(`${API_BASE}/videos/share/${shareToken}`);
	if (!res.ok) {
		const err = await res.json();
		throw new Error(err.error || 'Video not found');
	}
	return res.json();
}

const SUPPORTED_TYPES = [
	'video/mp4',
	'video/webm',
	'video/quicktime',
	'video/x-msvideo',
	'video/x-matroska'
];

const MAX_SIZE = 1_073_741_824; // 1GB

export function validateFile(file) {
	if (!SUPPORTED_TYPES.includes(file.type)) {
		return 'Unsupported file format. Accepted: MP4, WebM, MOV, AVI, MKV';
	}
	if (file.size > MAX_SIZE) {
		return 'File exceeds 1 GB limit';
	}
	return null;
}
