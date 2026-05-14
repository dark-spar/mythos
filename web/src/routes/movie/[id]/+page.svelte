<script lang="ts">
	import { onMount } from 'svelte';
	import Hls from 'hls.js';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import {
		browserCompatIssues,
		formatBytes,
		formatDuration,
		formatResolution,
		getMovie,
		putProgress,
		sendProgressBeacon,
		type CompatIssue,
		type MovieDetail
	} from '$lib/movies';

	let detail = $state<MovieDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let videoEl: HTMLVideoElement | undefined = $state();
	let initialPosition = $state<number | null>(null);
	let lastSavedAt = 0;
	let saveError = $state<string | null>(null);

	let usingHls = $state(false);
	let hls: Hls | null = null;

	const SAVE_INTERVAL_MS = 10_000;

	const id = $derived(page.params.id as string);

	$effect(() => {
		const currentId = id;
		(async () => {
			loading = true;
			error = null;
			detail = null;
			initialPosition = null;
			lastSavedAt = 0;
			try {
				detail = await getMovie(currentId);
				if (detail.progress && detail.progress.position_seconds > 1) {
					initialPosition = detail.progress.position_seconds;
				}
			} catch (e) {
				error =
					e instanceof ApiError
						? e.code === 'not_found'
							? 'That movie no longer exists.'
							: e.code.replace(/_/g, ' ')
						: e instanceof Error
							? e.message
							: 'failed to load movie';
			} finally {
				loading = false;
			}
		})();
	});

	// Attach a player implementation once both the <video> element and
	// the movie metadata are ready. Re-runs (and cleans up) when either
	// changes — navigating between movies, etc.
	$effect(() => {
		if (!videoEl || !detail) return;
		// Capture the id here so the cleanup closure can tell the server
		// which session to stop even after `detail` has been reset.
		const movieId = detail.movie.id;
		attachPlayer(videoEl, detail);
		return () => detachPlayer(movieId);
	});

	function attachPlayer(el: HTMLVideoElement, d: MovieDetail) {
		const issues = browserCompatIssues(d.file);
		if (issues.length === 0) {
			// Direct play: range-served raw file. The native <video>
			// element handles seeking by issuing fresh Range requests.
			usingHls = false;
			el.src = `/api/movies/${d.movie.id}/stream`;
			return;
		}

		// HLS fallback. The master describes every rendition; per-variant
		// playlists describe the full movie up front (synthetic VOD), so
		// seeking and resume work via the standard `currentTime = X`
		// path — the server starts/restarts the transcoder as the player
		// requests new segments.
		usingHls = true;
		const playlistUrl = `/api/movies/${d.movie.id}/hls/master.m3u8`;

		// Prefer hls.js whenever MSE is available. Some browsers
		// (Firefox on Android in particular) return non-empty
		// `canPlayType('application/vnd.apple.mpegurl')` even though
		// they can't actually decode an HLS manifest natively — letting
		// that branch win sends the .m3u8 to <video> directly, which
		// then errors with "no video with supported format and mime
		// type found". Native HLS is only the right answer on
		// Safari/iOS builds where MSE is unavailable.
		if (Hls.isSupported()) {
			hls = new Hls({
				enableWorker: true,
				lowLatencyMode: false,
				// Seeks restart ffmpeg, which takes 2-4s to produce the
				// first segment of the new position. hls.js's defaults
				// (20s timeout, 6 retries, nudge-forward on stall) tear
				// through that startup budget and cascade into the
				// timeline-corruption death spiral. Give the server
				// room to actually deliver, and don't move the playhead
				// while waiting.
				fragLoadingTimeOut: 35000,
				fragLoadingMaxRetry: 2,
				fragLoadingRetryDelay: 2000,
				manifestLoadingTimeOut: 10000,
				levelLoadingTimeOut: 10000,
				nudgeMaxRetry: 0,
				highBufferWatchdogPeriod: 30
			});
			hls.loadSource(playlistUrl);
			hls.attachMedia(el);
			return;
		}

		if (el.canPlayType('application/vnd.apple.mpegurl')) {
			// Safari iOS without MSE — fall back to the native player.
			el.src = playlistUrl;
			return;
		}

		saveError = "Your browser has no HLS support — can't play this file.";
	}

	function detachPlayer(movieId?: string) {
		if (hls) {
			hls.destroy();
			hls = null;
		}
		if (videoEl) {
			videoEl.removeAttribute('src');
			videoEl.load();
		}
		if (movieId) {
			stopTranscodeSession(movieId);
		}
	}

	function stopTranscodeSession(movieId: string) {
		try {
			// keepalive so the request survives page navigation / tab close.
			void fetch(`/api/movies/${movieId}/hls`, {
				method: 'DELETE',
				credentials: 'same-origin',
				keepalive: true
			});
		} catch {
			// page going away — nothing actionable
		}
	}

	function handleLoadedMetadata() {
		if (videoEl && initialPosition != null && !Number.isNaN(videoEl.duration)) {
			// Both transports now use absolute time on the <video> element
			// (HLS playlist is synthetic VOD describing the full movie),
			// so seek to the saved position once metadata is available.
			const target = Math.min(initialPosition, Math.max(0, videoEl.duration - 5));
			if (target > 0) {
				videoEl.currentTime = target;
			}
		}
	}

	function absolutePositionSeconds(): number | null {
		if (!videoEl || !Number.isFinite(videoEl.currentTime)) return null;
		return videoEl.currentTime;
	}

	function fullDurationSeconds(): number | null {
		if (detail?.file.duration_seconds && detail.file.duration_seconds > 0) {
			return detail.file.duration_seconds;
		}
		if (videoEl && Number.isFinite(videoEl.duration) && videoEl.duration > 0) {
			return videoEl.duration;
		}
		return null;
	}

	async function saveNow(reason: string) {
		if (!detail) return;
		const pos = absolutePositionSeconds();
		const dur = fullDurationSeconds();
		if (pos == null || dur == null || pos < 0 || dur <= 0) return;
		lastSavedAt = Date.now();
		try {
			await putProgress(detail.movie.id, pos, dur);
			saveError = null;
		} catch (e) {
			saveError =
				e instanceof Error ? `Couldn't save progress (${reason}): ${e.message}` : 'save failed';
		}
	}

	function handleTimeUpdate() {
		if (Date.now() - lastSavedAt >= SAVE_INTERVAL_MS) {
			void saveNow('tick');
		}
	}

	function handlePause() {
		void saveNow('pause');
	}

	function handleVisibilityChange() {
		if (!detail || !videoEl) return;
		if (document.hidden && !videoEl.paused) {
			const pos = absolutePositionSeconds();
			const dur = fullDurationSeconds();
			if (pos != null && dur != null && pos >= 0 && dur > 0) {
				sendProgressBeacon(detail.movie.id, pos, dur);
				lastSavedAt = Date.now();
			}
		}
	}

	function handleBeforeUnload() {
		if (!detail || !videoEl) return;
		const pos = absolutePositionSeconds();
		const dur = fullDurationSeconds();
		if (pos != null && dur != null && pos >= 0 && dur > 0) {
			sendProgressBeacon(detail.movie.id, pos, dur);
		}
		// Tear the server-side transcode session down as the tab closes.
		// The $effect cleanup also fires for in-app navigation; this
		// covers the full-tab-close case where Svelte doesn't get a
		// chance to run unmount handlers.
		stopTranscodeSession(detail.movie.id);
	}

	function toggleFullscreen() {
		if (!videoEl) return;
		if (document.fullscreenElement) {
			void document.exitFullscreen();
		} else {
			void videoEl.requestFullscreen();
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (!videoEl) return;
		const target = event.target as HTMLElement | null;
		if (
			target &&
			(target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
		) {
			return;
		}
		if (event.metaKey || event.ctrlKey || event.altKey) return;

		switch (event.key.toLowerCase()) {
			case 'f':
				event.preventDefault();
				toggleFullscreen();
				break;
			case ' ':
			case 'k':
				event.preventDefault();
				if (videoEl.paused) void videoEl.play();
				else videoEl.pause();
				break;
			case 'm':
				event.preventDefault();
				videoEl.muted = !videoEl.muted;
				break;
			case 'arrowright':
				event.preventDefault();
				videoEl.currentTime = Math.min(videoEl.duration || Infinity, videoEl.currentTime + 10);
				break;
			case 'arrowleft':
				event.preventDefault();
				videoEl.currentTime = Math.max(0, videoEl.currentTime - 10);
				break;
		}
	}

	function issueLabel(issue: CompatIssue): string {
		switch (issue.kind) {
			case 'container':
				return `Container .${issue.value}`;
			case 'video_codec':
				return `Video codec ${issue.value}`;
			case 'audio_codec':
				return `Audio codec ${issue.value}`;
		}
	}

	const compatIssues = $derived(detail ? browserCompatIssues(detail.file) : []);

	onMount(() => {
		document.addEventListener('visibilitychange', handleVisibilityChange);
		window.addEventListener('beforeunload', handleBeforeUnload);
		window.addEventListener('keydown', handleKeydown);
		return () => {
			document.removeEventListener('visibilitychange', handleVisibilityChange);
			window.removeEventListener('beforeunload', handleBeforeUnload);
			window.removeEventListener('keydown', handleKeydown);
		};
	});
</script>

<svelte:head>
	<title>{detail?.movie.title ?? 'Movie'} — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-5xl px-6 py-12">
	{#if detail}
		<a
			href={resolve(`/library/${detail.movie.library_id}`)}
			class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
		>
			← Back to library
		</a>
	{:else}
		<a
			href={resolve('/')}
			class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
		>
			← Home
		</a>
	{/if}

	{#if loading}
		<p class="mt-8 text-zinc-400">Loading…</p>
	{:else if error}
		<p class="mt-8 font-mono text-rose-500">{error}</p>
	{:else if detail}
		<section class="mt-4 overflow-hidden rounded-lg bg-black">
			<!-- Subtitle / caption tracks are a Phase 3+ feature; user-supplied
			     media has no guaranteed caption source. -->
			<video
				bind:this={videoEl}
				controls
				preload="metadata"
				class="aspect-video w-full"
				onloadedmetadata={handleLoadedMetadata}
				ontimeupdate={handleTimeUpdate}
				onpause={handlePause}
			>
				Your browser can't play this file directly.
			</video>
		</section>
		{#if compatIssues.length > 0}
			{#if usingHls}
				<div
					class="mt-2 rounded border border-sky-300 bg-sky-50 p-3 text-xs text-sky-900 dark:border-sky-700 dark:bg-sky-950 dark:text-sky-200"
					role="status"
				>
					<p>Transcoding this file on the fly because your browser can't decode it natively:</p>
					<ul class="mt-1 ml-4 list-disc">
						{#each compatIssues as issue (issue.kind)}
							<li><span class="font-mono">{issueLabel(issue)}</span></li>
						{/each}
					</ul>
					<p class="mt-2">
						First few seconds may take a moment as ffmpeg spins up. Big seeks restart the
						transcoder, so they pause briefly before resuming at the new spot.
					</p>
				</div>
			{:else}
				<div
					class="mt-2 rounded border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-700 dark:bg-amber-950 dark:text-amber-200"
					role="status"
				>
					<p>Your browser has no HLS support, and this file's codecs aren't natively playable:</p>
					<ul class="mt-1 ml-4 list-disc">
						{#each compatIssues as issue (issue.kind)}
							<li><span class="font-mono">{issueLabel(issue)}</span></li>
						{/each}
					</ul>
				</div>
			{/if}
		{/if}
		{#if saveError}
			<p class="mt-2 text-xs text-rose-500">{saveError}</p>
		{:else if initialPosition != null}
			<p class="mt-2 text-xs text-zinc-500">
				Resuming from {formatDuration(initialPosition)}. Press
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">f</kbd> for
				fullscreen,
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">space</kbd> to play/pause.
			</p>
		{:else}
			<p class="mt-2 text-xs text-zinc-500">
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">f</kbd> fullscreen ·
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">space</kbd> play/pause
				·
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">m</kbd> mute ·
				<kbd class="rounded border border-zinc-300 px-1 dark:border-zinc-700">←/→</kbd> seek 10s
			</p>
		{/if}

		<div class="mt-8 flex flex-col gap-6 sm:flex-row sm:items-start">
			{#if detail.movie.poster_url}
				<img
					src={detail.movie.poster_url}
					alt="{detail.movie.title} poster"
					class="w-32 shrink-0 rounded shadow-sm sm:w-40"
				/>
			{/if}
			<div class="min-w-0">
				<h1 class="text-3xl font-semibold tracking-tight">{detail.movie.title}</h1>
				{#if detail.movie.year != null}
					<p class="mt-1 text-zinc-500">{detail.movie.year}</p>
				{/if}

				{#if detail.movie.overview}
					<p class="mt-4 leading-relaxed text-zinc-700 dark:text-zinc-300">
						{detail.movie.overview}
					</p>
				{:else}
					<p class="mt-4 text-sm text-zinc-400 italic">
						No description on file. Configure a TMDb API key and rescan to enrich.
					</p>
				{/if}
			</div>
		</div>

		<section class="mt-10 rounded-lg border border-zinc-200 p-5 dark:border-zinc-800">
			<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">File</h2>
			<dl class="mt-4 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
				<dt class="text-zinc-500">Path</dt>
				<dd class="font-mono break-all text-zinc-900 dark:text-zinc-100">{detail.file.path}</dd>

				<dt class="text-zinc-500">Container</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{detail.file.container ?? '—'}</dd>

				<dt class="text-zinc-500">Video</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{detail.file.video_codec ?? '—'}</dd>

				<dt class="text-zinc-500">Audio</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{detail.file.audio_codec ?? '—'}</dd>

				<dt class="text-zinc-500">Resolution</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{formatResolution(detail.file) ?? '—'}</dd>

				<dt class="text-zinc-500">Duration</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">
					{formatDuration(detail.file.duration_seconds)}
				</dd>

				<dt class="text-zinc-500">Size</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{formatBytes(detail.file.size_bytes)}</dd>
			</dl>
		</section>
	{/if}
</main>
