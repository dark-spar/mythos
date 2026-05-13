<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import {
		formatBytes,
		formatDuration,
		formatResolution,
		getMovie,
		putProgress,
		sendProgressBeacon,
		type MovieDetail
	} from '$lib/movies';

	let detail = $state<MovieDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let videoEl: HTMLVideoElement | undefined = $state();
	let initialPosition = $state<number | null>(null);
	let lastSavedAt = 0;
	let saveError = $state<string | null>(null);

	/// How often during normal playback to write progress to the server.
	/// Pause / visibility-hidden / unload force an immediate write
	/// regardless of this interval.
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

	function handleLoadedMetadata() {
		if (videoEl && initialPosition != null && !Number.isNaN(videoEl.duration)) {
			// Don't auto-resume past the end. 5s margin so refreshes near
			// the credits don't reset to the start.
			const target = Math.min(initialPosition, Math.max(0, videoEl.duration - 5));
			if (target > 0) {
				videoEl.currentTime = target;
			}
		}
	}

	async function saveNow(reason: string) {
		if (!detail || !videoEl) return;
		const pos = videoEl.currentTime;
		const dur = videoEl.duration;
		if (!Number.isFinite(pos) || !Number.isFinite(dur) || dur <= 0) return;
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
			// Tab/window hidden during playback: write via keepalive
			// because the page may be backgrounded or terminated before a
			// regular fetch would complete.
			const pos = videoEl.currentTime;
			const dur = videoEl.duration;
			if (Number.isFinite(pos) && Number.isFinite(dur) && dur > 0) {
				sendProgressBeacon(detail.movie.id, pos, dur);
				lastSavedAt = Date.now();
			}
		}
	}

	function handleBeforeUnload() {
		if (!detail || !videoEl) return;
		const pos = videoEl.currentTime;
		const dur = videoEl.duration;
		if (Number.isFinite(pos) && Number.isFinite(dur) && dur > 0) {
			sendProgressBeacon(detail.movie.id, pos, dur);
		}
	}

	onMount(() => {
		document.addEventListener('visibilitychange', handleVisibilityChange);
		window.addEventListener('beforeunload', handleBeforeUnload);
		return () => {
			document.removeEventListener('visibilitychange', handleVisibilityChange);
			window.removeEventListener('beforeunload', handleBeforeUnload);
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
			<!-- svelte-ignore a11y_media_has_caption -->
			<!-- Subtitle / caption tracks are a Phase 3+ feature; user-supplied
			     media has no guaranteed caption source. -->
			<video
				bind:this={videoEl}
				controls
				preload="metadata"
				src={`/api/movies/${detail.movie.id}/stream`}
				class="aspect-video w-full"
				onloadedmetadata={handleLoadedMetadata}
				ontimeupdate={handleTimeUpdate}
				onpause={handlePause}
			>
				Your browser can't play this file directly. HLS transcoding lands in Phase 4.
			</video>
		</section>
		{#if saveError}
			<p class="mt-2 text-xs text-rose-500">{saveError}</p>
		{:else if initialPosition != null}
			<p class="mt-2 text-xs text-zinc-500">
				Resuming from {formatDuration(initialPosition)}.
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
