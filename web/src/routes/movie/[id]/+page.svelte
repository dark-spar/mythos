<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import {
		getMovie,
		formatBytes,
		formatDuration,
		formatResolution,
		type MovieDetail
	} from '$lib/movies';

	let detail = $state<MovieDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const id = $derived(page.params.id as string);

	$effect(() => {
		const currentId = id;
		(async () => {
			loading = true;
			error = null;
			try {
				detail = await getMovie(currentId);
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
</script>

<svelte:head>
	<title>{detail?.movie.title ?? 'Movie'} — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-3xl px-6 py-12">
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
		<div class="mt-4 flex flex-col gap-6 sm:flex-row sm:items-start">
			{#if detail.movie.poster_url}
				<img
					src={detail.movie.poster_url}
					alt="{detail.movie.title} poster"
					class="w-40 shrink-0 rounded shadow-sm sm:w-48"
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
