<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api';
	import { getLibrary, type Library } from '$lib/libraries';
	import { listMovies, type Movie } from '$lib/movies';

	let library = $state<Library | null>(null);
	let movies = $state<Movie[]>([]);
	let total = $state(0);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const id = $derived(page.params.id as string);

	$effect(() => {
		const currentId = id;
		(async () => {
			loading = true;
			error = null;
			try {
				const [lib, listing] = await Promise.all([
					getLibrary(currentId),
					listMovies(currentId, { limit: 200 })
				]);
				library = lib;
				movies = listing.items;
				total = listing.total;
			} catch (e) {
				error =
					e instanceof ApiError
						? e.code === 'not_found'
							? 'That library no longer exists.'
							: e.code.replace(/_/g, ' ')
						: e instanceof Error
							? e.message
							: 'failed to load library';
			} finally {
				loading = false;
			}
		})();
	});

	function yearLabel(year: number | null): string {
		return year != null ? `(${year})` : '';
	}
</script>

<svelte:head>
	<title>{library?.name ?? 'Library'} — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-6xl px-6 py-12">
	<a
		href={resolve('/')}
		class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
	>
		← All libraries
	</a>

	{#if loading}
		<p class="mt-8 text-zinc-400">Loading…</p>
	{:else if error}
		<p class="mt-8 font-mono text-rose-500">{error}</p>
	{:else if library}
		<header class="mt-4">
			<h1 class="text-3xl font-semibold tracking-tight">{library.name}</h1>
			<p class="mt-1 text-xs tracking-wide text-zinc-500 uppercase">
				{library.kind} · {total} item{total === 1 ? '' : 's'}
			</p>
		</header>

		{#if movies.length === 0}
			<p class="mt-12 text-zinc-500">
				No movies indexed yet.
				{#if total > 200}
					Showing first 200 of {total}.
				{:else}
					Run a scan from the admin page if files have been added.
				{/if}
			</p>
		{:else}
			<ul
				class="mt-10 grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
			>
				{#each movies as movie (movie.id)}
					<li>
						<a href={resolve(`/movie/${movie.id}`)} class="block transition hover:opacity-80">
							{#if movie.poster_url}
								<img
									src={movie.poster_url}
									alt="{movie.title} poster"
									loading="lazy"
									class="aspect-[2/3] w-full rounded bg-zinc-100 object-cover dark:bg-zinc-900"
								/>
							{:else}
								<div
									class="flex aspect-[2/3] items-center justify-center rounded bg-zinc-100 p-3 text-center text-xs text-zinc-400 dark:bg-zinc-900"
								>
									<span class="line-clamp-3 font-medium">{movie.title}</span>
								</div>
							{/if}
							<p
								class="mt-2 truncate text-sm font-medium text-zinc-900 dark:text-zinc-100"
								title={movie.title}
							>
								{movie.title}
							</p>
							{#if movie.year != null}
								<p class="text-xs text-zinc-500">{yearLabel(movie.year)}</p>
							{/if}
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</main>
