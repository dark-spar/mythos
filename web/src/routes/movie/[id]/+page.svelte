<script lang="ts">
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import {
		formatBytes,
		formatDuration,
		formatResolution,
		getMovie,
		type MovieDetail
	} from '$lib/movies';
	import Player from '$lib/player/Player.svelte';

	let detail = $state<MovieDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	async function loadDetail(movieId: string) {
		loading = true;
		error = null;
		detail = null;
		try {
			detail = await getMovie(movieId);
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
	}

	// `onMount` covers the initial mount; `afterNavigate` covers
	// subsequent client-side navs, including same-route ones where
	// $effect reading page.params.id did not re-trigger reliably.
	// Skipping `type === 'enter'` avoids double-fetching on first
	// load.
	onMount(() => {
		void loadDetail(page.params.id as string);
	});

	afterNavigate((nav) => {
		if (nav.type === 'enter') return;
		void loadDetail(page.params.id as string);
	});
</script>

<svelte:head>
	<title>{detail?.movie.title ?? 'Movie'} — Mythos</title>
</svelte:head>

<main class="min-h-screen bg-black pb-12 text-zinc-100">
	{#if loading}
		<div class="mx-auto max-w-5xl px-6 pt-12">
			<a
				href={resolve('/')}
				class="text-sm text-zinc-400 underline-offset-2 hover:text-zinc-100 hover:underline"
			>
				← Home
			</a>
		</div>
		<p class="mx-auto mt-8 max-w-5xl px-6 text-zinc-400">Loading…</p>
	{:else if error}
		<div class="mx-auto max-w-5xl px-6 pt-12">
			<a
				href={resolve('/')}
				class="text-sm text-zinc-400 underline-offset-2 hover:text-zinc-100 hover:underline"
			>
				← Home
			</a>
		</div>
		<p class="mx-auto mt-8 max-w-5xl px-6 font-mono text-rose-400">{error}</p>
	{:else if detail}
		{@const initialPosition =
			detail.progress && detail.progress.position_seconds > 1
				? detail.progress.position_seconds
				: null}
		<Player
			kind="movie"
			itemId={detail.movie.id}
			file={detail.file}
			subtitles={detail.subtitles}
			initialPositionSeconds={initialPosition}
			backHref={`/library/${detail.movie.library_id}`}
		/>

		<div class="mx-auto max-w-5xl px-6">
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
						<p class="mt-1 text-zinc-400">{detail.movie.year}</p>
					{/if}

					{#if detail.movie.overview}
						<p class="mt-4 leading-relaxed text-zinc-300">
							{detail.movie.overview}
						</p>
					{:else}
						<p class="mt-4 text-sm text-zinc-500 italic">
							No description on file. Configure a TMDb API key and rescan to enrich.
						</p>
					{/if}
				</div>
			</div>

			<section class="mt-10 rounded-lg border border-zinc-800 p-5">
				<h2 class="text-sm font-medium tracking-wide text-zinc-400 uppercase">File</h2>
				<dl class="mt-4 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
					<dt class="text-zinc-400">Path</dt>
					<dd class="font-mono break-all text-zinc-100">{detail.file.path}</dd>

					<dt class="text-zinc-400">Container</dt>
					<dd class="text-zinc-100">{detail.file.container ?? '—'}</dd>

					<dt class="text-zinc-400">Video</dt>
					<dd class="text-zinc-100">{detail.file.video_codec ?? '—'}</dd>

					<dt class="text-zinc-400">Audio</dt>
					<dd class="text-zinc-100">{detail.file.audio_codec ?? '—'}</dd>

					<dt class="text-zinc-400">Resolution</dt>
					<dd class="text-zinc-100">{formatResolution(detail.file) ?? '—'}</dd>

					<dt class="text-zinc-400">Duration</dt>
					<dd class="text-zinc-100">
						{formatDuration(detail.file.duration_seconds)}
					</dd>

					<dt class="text-zinc-400">Size</dt>
					<dd class="text-zinc-100">{formatBytes(detail.file.size_bytes)}</dd>
				</dl>
			</section>
		</div>
	{/if}
</main>
