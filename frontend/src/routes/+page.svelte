<script>
	import { initiateUpload, uploadToStorage, completeUpload, getVideoStatus, validateFile } from '$lib/api.js';

	let title = $state('');
	let file = $state(null);
	let dragOver = $state(false);
	let status = $state('idle'); // idle, validating, uploading, completing, processing, done, error
	let progress = $state(0);
	let shareUrl = $state(null);
	let errorMessage = $state(null);

	function handleDrop(e) {
		e.preventDefault();
		dragOver = false;
		const dropped = e.dataTransfer?.files?.[0];
		if (dropped) file = dropped;
	}

	function handleFileSelect(e) {
		file = e.target.files?.[0] || null;
	}

	async function handleUpload() {
		if (!file || !title.trim()) return;

		errorMessage = null;
		status = 'validating';

		// Frontend validation
		const validationError = validateFile(file);
		if (validationError) {
			errorMessage = validationError;
			status = 'error';
			return;
		}

		try {
			// 1. Initiate
			status = 'uploading';
			const initResult = await initiateUpload(title.trim(), file.type, file.name);
			const videoId = initResult.id;

			// 2. Upload to storage
			await uploadToStorage(initResult.upload_url, file, initResult.upload_headers);

			// 3. Complete
			status = 'completing';
			await completeUpload(videoId);

			// 4. Poll for share URL
			status = 'processing';
			shareUrl = await pollForShareUrl(videoId);
			status = 'done';
		} catch (err) {
			errorMessage = err.message;
			status = 'error';
		}
	}

	async function pollForShareUrl(videoId) {
		const maxAttempts = 120; // 10 minutes at 5s intervals
		for (let i = 0; i < maxAttempts; i++) {
			const result = await getVideoStatus(videoId);
			if (result.share_url) {
				return result.share_url;
			}
			if (result.status === 'Failed') {
				throw new Error(result.error_message || 'Processing failed');
			}
			await new Promise((r) => setTimeout(r, 5000));
		}
		throw new Error('Processing timed out');
	}

	function reset() {
		title = '';
		file = null;
		status = 'idle';
		progress = 0;
		shareUrl = null;
		errorMessage = null;
	}

	function copyLink() {
		if (shareUrl) navigator.clipboard.writeText(shareUrl);
	}
</script>

<main>
	<h1>benri-stream</h1>
	<p class="subtitle">Upload a video, get a shareable link.</p>

	{#if status === 'done' && shareUrl}
		<div class="result">
			<p>Your video is ready!</p>
			<div class="link-box">
				<input type="text" readonly value={shareUrl} />
				<button onclick={copyLink}>Copy</button>
			</div>
			<button class="secondary" onclick={reset}>Upload another</button>
		</div>
	{:else}
		<div
			class="drop-zone"
			class:drag-over={dragOver}
			ondragover={(e) => { e.preventDefault(); dragOver = true; }}
			ondragleave={() => dragOver = false}
			ondrop={handleDrop}
		>
			{#if file}
				<p class="file-name">{file.name}</p>
				<p class="file-size">{(file.size / 1_000_000).toFixed(1)} MB</p>
			{:else}
				<p>Drop a video file here</p>
				<p class="hint">or</p>
				<label class="file-label">
					Browse
					<input type="file" accept="video/*" onchange={handleFileSelect} hidden />
				</label>
			{/if}
		</div>

		<input
			type="text"
			class="title-input"
			placeholder="Video title"
			bind:value={title}
			disabled={status !== 'idle'}
		/>

		{#if errorMessage}
			<p class="error">{errorMessage}</p>
		{/if}

		<button
			class="upload-btn"
			onclick={handleUpload}
			disabled={!file || !title.trim() || (status !== 'idle' && status !== 'error')}
		>
			{#if status === 'uploading'}
				Uploading...
			{:else if status === 'completing'}
				Finalizing...
			{:else if status === 'processing'}
				Processing... (waiting for link)
			{:else}
				Upload
			{/if}
		</button>
	{/if}
</main>

<style>
	:global(body) {
		margin: 0;
		font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
		background: #0a0a0a;
		color: #e0e0e0;
	}

	main {
		max-width: 540px;
		margin: 80px auto;
		padding: 0 20px;
	}

	h1 {
		font-size: 2rem;
		font-weight: 600;
		margin-bottom: 4px;
	}

	.subtitle {
		color: #888;
		margin-bottom: 32px;
	}

	.drop-zone {
		border: 2px dashed #333;
		border-radius: 12px;
		padding: 48px 24px;
		text-align: center;
		cursor: pointer;
		transition: border-color 0.2s;
	}

	.drop-zone.drag-over {
		border-color: #4a9eff;
		background: rgba(74, 158, 255, 0.05);
	}

	.file-name {
		font-weight: 500;
		word-break: break-all;
	}

	.file-size {
		color: #888;
		font-size: 0.9rem;
	}

	.hint {
		color: #555;
		font-size: 0.85rem;
		margin: 8px 0;
	}

	.file-label {
		display: inline-block;
		padding: 8px 20px;
		background: #1a1a1a;
		border: 1px solid #333;
		border-radius: 6px;
		cursor: pointer;
	}

	.file-label:hover {
		background: #222;
	}

	.title-input {
		width: 100%;
		padding: 12px 16px;
		margin-top: 16px;
		background: #111;
		border: 1px solid #333;
		border-radius: 8px;
		color: #e0e0e0;
		font-size: 1rem;
		box-sizing: border-box;
	}

	.upload-btn {
		width: 100%;
		padding: 14px;
		margin-top: 16px;
		background: #4a9eff;
		color: white;
		border: none;
		border-radius: 8px;
		font-size: 1rem;
		font-weight: 500;
		cursor: pointer;
	}

	.upload-btn:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.upload-btn:hover:not(:disabled) {
		background: #3a8eef;
	}

	.error {
		color: #ff4a4a;
		font-size: 0.9rem;
		margin-top: 8px;
	}

	.result {
		text-align: center;
	}

	.result p {
		font-size: 1.2rem;
		margin-bottom: 16px;
	}

	.link-box {
		display: flex;
		gap: 8px;
		margin-bottom: 16px;
	}

	.link-box input {
		flex: 1;
		padding: 12px;
		background: #111;
		border: 1px solid #333;
		border-radius: 8px;
		color: #e0e0e0;
		font-size: 0.9rem;
	}

	.link-box button {
		padding: 12px 20px;
		background: #4a9eff;
		color: white;
		border: none;
		border-radius: 8px;
		cursor: pointer;
		font-weight: 500;
	}

	.secondary {
		background: none;
		border: 1px solid #333;
		color: #888;
		padding: 10px 20px;
		border-radius: 8px;
		cursor: pointer;
	}

	.secondary:hover {
		color: #e0e0e0;
		border-color: #555;
	}
</style>
