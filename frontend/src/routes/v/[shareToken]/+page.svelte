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

<main class="max-w-5xl mx-auto px-5 py-10">
	{#if error}
		<div class="text-center pt-30">
			<h1 class="text-2xl font-semibold mb-4">benri-stream</h1>
			<p class="text-red-500">{error}</p>
			<a href="/" class="text-sky-500 no-underline hover:underline">Upload a video</a>
		</div>
	{:else if !video}
		<div class="text-center pt-30">Loading...</div>
	{:else if video.status === 'Failed'}
		<div class="text-center pt-30">
			<h1 class="text-2xl font-semibold mb-4">{video.title}</h1>
			<p class="text-red-500">
				{video.error_message || 'This video could not be processed.'}
			</p>
		</div>
	{:else if video.status === 'Processing'}
		<div class="text-center pt-30">
			<h1 class="text-2xl font-semibold mb-4">{video.title}</h1>
			<p class="text-neutral-500">Processing... video will be available shortly.</p>
			<div
				class="w-8 h-8 border-[3px] border-neutral-800 border-t-sky-500 rounded-full mx-auto my-6 animate-spin"
			></div>
		</div>
	{:else}
		<div>
			<h1 class="text-2xl font-semibold mb-4">{video.title}</h1>
			<div class="relative w-full bg-black rounded-lg overflow-hidden">
				<video
					bind:this={videoEl}
					controls
					autoplay
					playsinline
					class="w-full block"
				></video>
			</div>
		</div>
	{/if}
</main>
