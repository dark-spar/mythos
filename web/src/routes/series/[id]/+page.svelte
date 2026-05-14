<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import {
		getSeason,
		getSeries,
		seasonLabel,
		type Episode,
		type Season,
		type Series
	} from '$lib/tv';

	let series = $state<Series | null>(null);
	let seasons = $state<Season[]>([]);
	let episodesBySeason = $state<Record<string, Episode[]>>({});
	let loading = $state(true);
	let error = $state<string | null>(null);

	const id = $derived(page.params.id as string);

	$effect(() => {
		const currentId = id;
		(async () => {
			loading = true;
			error = null;
			episodesBySeason = {};
			try {
				const detail = await getSeries(currentId);
				series = detail.series;
				seasons = detail.seasons;

				const all = await Promise.all(
					detail.seasons.map((s) =>
						getSeason(detail.series.id, s.season_number).then((sd) => ({
							id: s.id,
							episodes: sd.episodes
						}))
					)
				);
				const map: Record<string, Episode[]> = {};
				for (const { id: seasonId, episodes } of all) {
					map[seasonId] = episodes;
				}
				episodesBySeason = map;
			} catch (e) {
				error =
					e instanceof ApiError
						? e.code === 'not_found'
							? 'That series no longer exists.'
							: e.code.replace(/_/g, ' ')
						: e instanceof Error
							? e.message
							: 'failed to load series';
			} finally {
				loading = false;
			}
		})();
	});

	function episodeLabel(ep: Episode): string {
		const num = ep.episode_number.toString().padStart(2, '0');
		return ep.title ? `E${num} — ${ep.title}` : `E${num}`;
	}
</script>

<svelte:head>
	<title>{series?.title ?? 'Series'} — Mythos</title>
</svelte:head>

<main class="px-6 py-12 sm:px-8">
	<a
		href={series ? resolve(`/library/${series.library_id}`) : resolve('/')}
		class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
	>
		← Back to library
	</a>

	{#if loading}
		<p class="mt-8 text-zinc-400">Loading…</p>
	{:else if error}
		<p class="mt-8 font-mono text-rose-500">{error}</p>
	{:else if series}
		<header class="mt-6 flex flex-col gap-6 sm:flex-row">
			{#if series.poster_url}
				<img
					src={series.poster_url}
					alt="{series.title} poster"
					class="aspect-[2/3] w-40 shrink-0 rounded bg-zinc-100 object-cover dark:bg-zinc-900"
				/>
			{:else}
				<div
					class="flex aspect-[2/3] w-40 shrink-0 items-center justify-center rounded bg-zinc-100 p-3 text-center text-xs text-zinc-400 dark:bg-zinc-900"
				>
					<span class="line-clamp-4 font-medium">{series.title}</span>
				</div>
			{/if}

			<div class="min-w-0 flex-1">
				<h1 class="text-3xl font-semibold tracking-tight">{series.title}</h1>
				<p class="mt-1 text-xs tracking-wide text-zinc-500 uppercase">
					{series.year != null ? series.year : 'year unknown'} · {seasons.length} season{seasons.length ===
					1
						? ''
						: 's'}
				</p>
				{#if series.overview}
					<p class="mt-4 max-w-prose text-sm leading-relaxed text-zinc-700 dark:text-zinc-300">
						{series.overview}
					</p>
				{/if}
			</div>
		</header>

		{#if seasons.length === 0}
			<p class="mt-12 text-zinc-500">No seasons indexed yet.</p>
		{:else}
			<div class="mt-10 space-y-10">
				{#each seasons as season (season.id)}
					<section>
						<h2 class="text-xl font-semibold tracking-tight">
							{seasonLabel(season)}
						</h2>
						{#if season.overview}
							<p class="mt-2 max-w-prose text-sm text-zinc-600 dark:text-zinc-400">
								{season.overview}
							</p>
						{/if}

						{#if episodesBySeason[season.id]?.length}
							<ul class="mt-4 divide-y divide-zinc-200 dark:divide-zinc-800">
								{#each episodesBySeason[season.id] as ep (ep.id)}
									<li>
										<a
											href={resolve(`/episodes/${ep.id}`)}
											class="flex items-start gap-3 py-3 transition hover:opacity-80"
										>
											{#if ep.still_url}
												<img
													src={ep.still_url}
													alt=""
													loading="lazy"
													class="aspect-video w-32 shrink-0 rounded bg-zinc-100 object-cover dark:bg-zinc-900"
												/>
											{:else}
												<div
													class="aspect-video w-32 shrink-0 rounded bg-zinc-100 dark:bg-zinc-900"
												></div>
											{/if}
											<div class="min-w-0 flex-1">
												<p class="text-sm font-medium text-zinc-900 dark:text-zinc-100">
													{episodeLabel(ep)}
												</p>
												{#if ep.air_date}
													<p class="text-xs text-zinc-500">{ep.air_date}</p>
												{/if}
												{#if ep.overview}
													<p
														class="mt-1 line-clamp-3 max-w-prose text-xs text-zinc-600 dark:text-zinc-400"
													>
														{ep.overview}
													</p>
												{/if}
											</div>
										</a>
									</li>
								{/each}
							</ul>
						{:else}
							<p class="mt-4 text-sm text-zinc-500">No episodes indexed yet.</p>
						{/if}
					</section>
				{/each}
			</div>
		{/if}
	{/if}
</main>
