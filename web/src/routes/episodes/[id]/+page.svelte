<script lang="ts">
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import { formatBytes, formatDuration, formatResolution } from '$lib/movies';
	import Player from '$lib/player/Player.svelte';
	import { getEpisode, seasonLabel, type EpisodeDetail } from '$lib/tv';

	let detail = $state<EpisodeDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	/// Latched per loadDetail call. The Player consumes it on mount;
	/// after that point the value being stale is fine because the
	/// Player only reads it once (in `tryAutoplay`).
	let autoplayThisNav = $state(false);

	async function loadDetail(episodeId: string, shouldAutoplay: boolean) {
		loading = true;
		error = null;
		detail = null;
		autoplayThisNav = false;
		try {
			detail = await getEpisode(episodeId);
			autoplayThisNav = shouldAutoplay;
		} catch (e) {
			error =
				e instanceof ApiError
					? e.code === 'not_found'
						? 'That episode no longer exists.'
						: e.code.replace(/_/g, ' ')
					: e instanceof Error
						? e.message
						: 'failed to load episode';
		} finally {
			loading = false;
		}
	}

	// `onMount` covers the initial mount (and hard refresh after the
	// layout's auth gate releases this page). `afterNavigate` covers
	// subsequent client-side navs, including same-route ones where
	// a $effect reading page.params.id wasn't re-firing reliably.
	// `type === 'enter'` is skipped to avoid double-fetching on
	// initial mount.
	//
	// The Player auto-starts playback only when arrived-at-via the
	// up-next countdown (signalled through SvelteKit page state).
	// Manual navigation (click a link, refresh, type the URL) leaves
	// `page.state.autoplay` unset, so onMount passes false.
	onMount(() => {
		void loadDetail(page.params.id as string, false);
	});

	afterNavigate((nav) => {
		if (nav.type === 'enter') return;
		void loadDetail(page.params.id as string, page.state.autoplay === true);
	});

	function episodeLabel(seasonNumber: number, episodeNumber: number): string {
		return `S${seasonNumber.toString().padStart(2, '0')}E${episodeNumber.toString().padStart(2, '0')}`;
	}
</script>

<svelte:head>
	<title>
		{detail
			? `${detail.series.title} — ${episodeLabel(detail.season.season_number, detail.episode.episode_number)}`
			: 'Episode'} — Mythos
	</title>
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
		{@const upNext = detail.next
			? {
					href: `/episodes/${detail.next.id}`,
					label: `${episodeLabel(detail.next.season_number, detail.next.episode_number)}${detail.next.title ? ` — ${detail.next.title}` : ''}`
				}
			: undefined}
		<Player
			kind="episode"
			itemId={detail.episode.id}
			file={detail.file}
			subtitles={detail.subtitles}
			initialPositionSeconds={initialPosition}
			backHref={`/series/${detail.series.id}`}
			next={upNext}
			autoplayOnMount={autoplayThisNav}
		/>

		<div class="mx-auto max-w-5xl px-6 pt-6">
			<nav class="text-xs text-zinc-400">
				<a
					href={resolve(`/library/${detail.series.library_id}`)}
					class="underline-offset-2 hover:text-zinc-100 hover:underline">{detail.series.title}</a
				>
				<span class="mx-1">·</span>
				<a
					href={resolve(`/series/${detail.series.id}`)}
					class="underline-offset-2 hover:text-zinc-100 hover:underline"
					>{seasonLabel(detail.season)}</a
				>
				<span class="mx-1">·</span>
				<span>{episodeLabel(detail.season.season_number, detail.episode.episode_number)}</span>
			</nav>

			<header class="mt-3 flex flex-col gap-4 sm:flex-row sm:items-start">
				{#if detail.episode.still_url}
					<img
						src={detail.episode.still_url}
						alt=""
						class="aspect-video w-48 shrink-0 rounded bg-zinc-900 object-cover"
					/>
				{/if}
				<div class="min-w-0 flex-1">
					<h1 class="text-2xl font-semibold tracking-tight">
						{detail.episode.title ??
							episodeLabel(detail.season.season_number, detail.episode.episode_number)}
					</h1>
					{#if detail.episode.air_date}
						<p class="mt-1 text-sm text-zinc-400">{detail.episode.air_date}</p>
					{/if}
					{#if detail.episode.overview}
						<p class="mt-3 max-w-prose text-sm leading-relaxed text-zinc-300">
							{detail.episode.overview}
						</p>
					{/if}
				</div>
			</header>

			<nav class="mt-6 flex items-center justify-between gap-3 text-sm">
				{#if detail.prev}
					<a
						href={resolve(`/episodes/${detail.prev.id}`)}
						class="text-zinc-300 underline-offset-2 hover:text-zinc-100 hover:underline"
					>
						← Previous · E{detail.prev.episode_number.toString().padStart(2, '0')}{detail.prev.title
							? ` — ${detail.prev.title}`
							: ''}
					</a>
				{:else}
					<span></span>
				{/if}
				{#if detail.next}
					<a
						href={resolve(`/episodes/${detail.next.id}`)}
						class="text-zinc-300 underline-offset-2 hover:text-zinc-100 hover:underline"
					>
						Next · E{detail.next.episode_number.toString().padStart(2, '0')}{detail.next.title
							? ` — ${detail.next.title}`
							: ''} →
					</a>
				{/if}
			</nav>

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
					<dd class="text-zinc-100">{formatDuration(detail.file.duration_seconds)}</dd>

					<dt class="text-zinc-400">Size</dt>
					<dd class="text-zinc-100">{formatBytes(detail.file.size_bytes)}</dd>
				</dl>
			</section>
		</div>
	{/if}
</main>
