<script>
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { getVideoByToken } from '$lib/api.js';

	let video = $state(null);
	let error = $state(null);
	let videoEl = $state(null);

	const shareToken = $derived($page.params.shareToken);

	onMount(async () => {
		try {
			video = await getVideoByToken(shareToken);
			if (video.stream_url) {
				initPlayer(video.stream_url);
			} else if (video.status === 'Processing') {
				pollUntilReady();
			}
		} catch (err) {
			error = err.message;
		}
	});

	async function pollUntilReady() {
		const interval = setInterval(async () => {
			try {
				video = await getVideoByToken(shareToken);
				if (video.stream_url) {
					clearInterval(interval);
					initPlayer(video.stream_url);
				}
				if (video.status === 'Failed') {
					clearInterval(interval);
				}
			} catch (err) {
				clearInterval(interval);
				error = err.message;
			}
		}, 3000);
	}

	async function initPlayer(streamUrl) {
		// Wait for video element to be mounted
		await new Promise((r) => setTimeout(r, 100));
		if (!videoEl) return;

		if (videoEl.canPlayType('application/vnd.apple.mpegurl')) {
			// Native HLS support (Safari)
			videoEl.src = streamUrl;
		} else {
			// Use hls.js for other browsers
			const { default: Hls } = await import('hls.js');
			if (Hls.isSupported()) {
				const hls = new Hls();
				hls.loadSource(streamUrl);
				hls.attachMedia(videoEl);
			} else {
				error = 'Your browser does not support HLS video playback.';
			}
		}
	}
</script>

<main>
	{#if error}
		<div class="error-page">
			<h1>benri-stream</h1>
			<p class="error">{error}</p>
			<a href="/">Upload a video</a>
		</div>
	{:else if !video}
		<div class="loading">Loading...</div>
	{:else if video.status === 'Failed'}
		<div class="error-page">
			<h1>{video.title}</h1>
			<p class="error">{video.error_message || 'This video could not be processed.'}</p>
		</div>
	{:else if video.status === 'Processing'}
		<div class="processing">
			<h1>{video.title}</h1>
			<p>Processing... video will be available shortly.</p>
			<div class="spinner"></div>
		</div>
	{:else}
		<div class="player-page">
			<h1>{video.title}</h1>
			<div class="player-wrapper">
				<video
					bind:this={videoEl}
					controls
					autoplay
					playsinline
				></video>
			</div>
		</div>
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
		max-width: 960px;
		margin: 0 auto;
		padding: 40px 20px;
	}

	h1 {
		font-size: 1.5rem;
		font-weight: 600;
		margin-bottom: 16px;
	}

	.player-wrapper {
		position: relative;
		width: 100%;
		background: #000;
		border-radius: 8px;
		overflow: hidden;
	}

	video {
		width: 100%;
		display: block;
	}

	.status-note {
		color: #888;
		font-size: 0.85rem;
		margin-top: 8px;
	}

	.loading, .processing {
		text-align: center;
		padding-top: 120px;
	}

	.processing p {
		color: #888;
	}

	.spinner {
		width: 32px;
		height: 32px;
		border: 3px solid #333;
		border-top-color: #4a9eff;
		border-radius: 50%;
		margin: 24px auto;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to { transform: rotate(360deg); }
	}

	.error-page {
		text-align: center;
		padding-top: 120px;
	}

	.error {
		color: #ff4a4a;
	}

	a {
		color: #4a9eff;
		text-decoration: none;
	}

	a:hover {
		text-decoration: underline;
	}
</style>
