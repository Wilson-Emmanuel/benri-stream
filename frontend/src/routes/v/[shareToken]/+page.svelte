<script>
	import { page } from '$app/stores';
	import { get } from 'svelte/store';
	import { getVideoByToken } from '$lib/api.js';

	let video = $state(null);
	let error = $state(null);
	let videoEl = $state(null);

	// Read the route param synchronously from the page store. The
	// store is already populated by the time this component script
	// runs, and avoiding `$derived($page.params.shareToken)` keeps
	// the read out of Svelte 5's reactive graph — which otherwise
	// interacts badly with an async `onMount` and silently swallows
	// the rejection.
	const shareToken = get(page).params.shareToken;

	// Kick off the fetch via `$effect` rather than `onMount`. In a
	// pure-runes component `onMount` can silently fail to fire (it's
	// the legacy lifecycle hook, kept around for compatibility but
	// not wired into the runes mount path the same way). `$effect`
	// runs once after the component is mounted to the DOM, which is
	// exactly what we need, and is the idiomatic Svelte 5 API.
	$effect(() => {
		loadAndPlay();
	});

	async function loadAndPlay() {
		try {
			video = await getVideoByToken(shareToken);
			if (video.stream_url) {
				initPlayer(video.stream_url);
			} else if (video.status === 'PROCESSING' || video.status === 'UPLOADED') {
				pollUntilReady();
			}
		} catch (err) {
			error = err.message;
		}
	}

	async function pollUntilReady() {
		const interval = setInterval(async () => {
			try {
				video = await getVideoByToken(shareToken);
				if (video.stream_url) {
					clearInterval(interval);
					initPlayer(video.stream_url);
				}
				if (video.status === 'FAILED') {
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
				// `startLevel: 0` pins the player to the lowest-quality
				// variant on first load. Important during early publish:
				// the medium and high variant playlists may not exist
				// in storage yet at the moment the viewer opens the
				// share link, and hls.js's default auto-start would
				// otherwise try to pick a mid-tier based on its initial
				// bandwidth guess and 404. Starting at low guarantees
				// a playable stream; hls.js still adapts upward once
				// higher tiers become available and bandwidth allows.
				//
				// During early publish the variant playlist is an
				// `EVENT` playlist with no `#EXT-X-ENDLIST` yet, which
				// hls.js classifies as a live stream. Its default for
				// live is to seek to the live edge (the most recently
				// appended segment), so every fresh page load lands on
				// a different scene while the clock reads 0:00 — that's
				// the bug the user sees as "random scenes on refresh".
				//
				// The `Hls` config option `startPosition: 0` is a soft
				// hint that live-stream startup logic overrides. The
				// reliable override is to disable auto-start, wait for
				// the manifest to parse, then call `startLoad(0)` —
				// which pins the loader to absolute position 0 no
				// matter how hls.js classified the playlist. We also
				// push `currentTime = 0` on the video element as belt
				// and suspenders so the media element itself doesn't
				// race ahead to the live edge.
				const hls = new Hls({
					startLevel: 0,
					autoStartLoad: false,
					debug: true,
				});

				// Comprehensive event logging so we can see what hls.js
				// is doing while we debug the early-publish playback
				// path. Remove once playback is reliable.
				hls.on(Hls.Events.MANIFEST_PARSED, (_, data) => {
					console.log('[hls] MANIFEST_PARSED', data);
					// Kick off loading pinned to absolute position 0.
					// Paired with `autoStartLoad: false` above, this
					// is the only thing that reliably prevents live-
					// edge startup for EVENT playlists in hls.js.
					hls.startLoad(0);
					if (videoEl) videoEl.currentTime = 0;
				});
				hls.on(Hls.Events.LEVEL_LOADED, (_, data) =>
					console.log('[hls] LEVEL_LOADED', {
						level: data.level,
						url: data.details?.url,
						live: data.details?.live,
						totalduration: data.details?.totalduration,
						fragments: data.details?.fragments?.length,
						endSN: data.details?.endSN,
					})
				);
				hls.on(Hls.Events.FRAG_LOADING, (_, data) =>
					console.log('[hls] FRAG_LOADING', data.frag?.url)
				);
				hls.on(Hls.Events.FRAG_LOADED, (_, data) =>
					console.log('[hls] FRAG_LOADED', data.frag?.url)
				);
				hls.on(Hls.Events.ERROR, (_, data) =>
					console.error('[hls] ERROR', {
						type: data.type,
						details: data.details,
						fatal: data.fatal,
						url: data.url,
						response: data.response,
					})
				);

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
	{:else if video.status === 'FAILED'}
		<div class="text-center pt-30">
			<h1 class="text-2xl font-semibold mb-4">{video.title}</h1>
			<p class="text-red-500">
				{video.error_message || 'This video could not be processed.'}
			</p>
		</div>
	{:else if video.status === 'PROCESSING' || video.status === 'UPLOADED'}
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
