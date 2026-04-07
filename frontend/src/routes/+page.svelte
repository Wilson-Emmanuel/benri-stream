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
			if (result.status === 'FAILED') {
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

<main class="max-w-[540px] mx-auto my-20 px-5">
	<h1 class="text-3xl font-semibold mb-1">benri-stream</h1>
	<p class="text-neutral-500 mb-8">Upload a video, get a shareable link.</p>

	{#if status === 'done' && shareUrl}
		<div class="text-center">
			<p class="text-xl mb-4">Your video is ready!</p>
			<div class="flex gap-2 mb-4">
				<input
					type="text"
					readonly
					value={shareUrl}
					class="flex-1 p-3 bg-neutral-950 border border-neutral-800 rounded-lg text-neutral-200 text-sm"
				/>
				<button
					onclick={copyLink}
					class="px-5 py-3 bg-sky-500 text-white border-none rounded-lg cursor-pointer font-medium hover:bg-sky-600"
				>
					Copy
				</button>
			</div>
			<button
				onclick={reset}
				class="bg-transparent border border-neutral-800 text-neutral-500 px-5 py-2.5 rounded-lg cursor-pointer hover:text-neutral-200 hover:border-neutral-600"
			>
				Upload another
			</button>
		</div>
	{:else}
		<div
			class="border-2 border-dashed rounded-xl px-6 py-12 text-center cursor-pointer transition-colors {dragOver
				? 'border-sky-500 bg-sky-500/5'
				: 'border-neutral-800'}"
			ondragover={(e) => {
				e.preventDefault();
				dragOver = true;
			}}
			ondragleave={() => (dragOver = false)}
			ondrop={handleDrop}
		>
			{#if file}
				<p class="font-medium break-all">{file.name}</p>
				<p class="text-neutral-500 text-sm">{(file.size / 1_000_000).toFixed(1)} MB</p>
			{:else}
				<p>Drop a video file here</p>
				<p class="text-neutral-600 text-sm my-2">or</p>
				<label class="inline-block px-5 py-2 bg-neutral-900 border border-neutral-800 rounded-md cursor-pointer hover:bg-neutral-800">
					Browse
					<input type="file" accept="video/*" onchange={handleFileSelect} hidden />
				</label>
			{/if}
		</div>

		<input
			type="text"
			class="w-full px-4 py-3 mt-4 bg-neutral-950 border border-neutral-800 rounded-lg text-neutral-200 text-base box-border"
			placeholder="Video title"
			bind:value={title}
			disabled={status !== 'idle'}
		/>

		{#if errorMessage}
			<p class="text-red-500 text-sm mt-2">{errorMessage}</p>
		{/if}

		<button
			class="w-full py-3.5 mt-4 bg-sky-500 text-white border-none rounded-lg text-base font-medium cursor-pointer hover:bg-sky-600 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-sky-500"
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
