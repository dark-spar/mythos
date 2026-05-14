<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { auth } from '$lib/auth.svelte';
	import { listLibraries, type Library } from '$lib/libraries';
	import {
		listContinueWatching,
		primaryTitle,
		progressFraction,
		subtitleText,
		type ContinueWatchingItem
	} from '$lib/continue-watching';

	let libraries = $state<Library[]>([]);
	let continueWatching = $state<ContinueWatchingItem[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [libs, cw] = await Promise.all([listLibraries(), listContinueWatching(24)]);
			libraries = libs;
			continueWatching = cw;
		} catch (e) {
			error = e instanceof Error ? e.message : 'failed to load home';
		} finally {
			loading = false;
		}
	});
</script>

<svelte:head>
	<title>Mythos</title>
</svelte:head>

<main class="mx-auto max-w-4xl px-6 py-12">
	<header class="flex items-baseline justify-between">
		<div>
			<h1 class="text-4xl font-semibold tracking-tight">Mythos</h1>
			<p class="mt-2 text-sm text-zinc-500">A self-hosted media server, written in Rust.</p>
		</div>
		{#if auth.user}
			<div class="text-right text-sm">
				<p class="text-zinc-900 dark:text-zinc-100">{auth.user.username}</p>
				<div class="mt-1 flex justify-end gap-3 text-zinc-500">
					{#if auth.user.is_admin}
						<a
							href={resolve('/admin/libraries')}
							class="underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
						>
							Libraries
						</a>
						<a
							href={resolve('/admin/settings')}
							class="underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
						>
							Settings
						</a>
					{/if}
					<button
						class="underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
						onclick={() => auth.logout()}
					>
						Sign out
					</button>
				</div>
			</div>
		{/if}
	</header>

	{#if !loading && continueWatching.length > 0}
		<section class="mt-12">
			<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">Continue watching</h2>
			<ul class="mt-4 flex gap-4 overflow-x-auto pb-2 sm:gap-5">
				{#each continueWatching as item (item.kind + ':' + item.id)}
					{@const frac = progressFraction(item)}
					<li class="w-36 shrink-0 sm:w-40">
						<a
							href={item.kind === 'movie'
								? resolve(`/movie/${item.id}`)
								: resolve(`/episodes/${item.id}`)}
							class="block transition hover:opacity-80"
							aria-label={`Resume ${primaryTitle(item)}`}
						>
							<div
								class="relative aspect-[2/3] w-full overflow-hidden rounded bg-zinc-100 dark:bg-zinc-900"
							>
								{#if item.poster_url}
									<img
										src={item.poster_url}
										alt=""
										loading="lazy"
										class="h-full w-full object-cover"
									/>
								{:else}
									<div
										class="flex h-full w-full items-center justify-center p-3 text-center text-xs text-zinc-400"
									>
										<span class="line-clamp-3 font-medium">{primaryTitle(item)}</span>
									</div>
								{/if}
								<!-- progress bar at the bottom of the poster -->
								<div class="absolute inset-x-0 bottom-0 h-1 bg-black/30">
									<div class="h-full bg-rose-500" style="width: {(frac * 100).toFixed(1)}%"></div>
								</div>
							</div>
							<p
								class="mt-2 truncate text-sm font-medium text-zinc-900 dark:text-zinc-100"
								title={primaryTitle(item)}
							>
								{primaryTitle(item)}
							</p>
							<p class="truncate text-xs text-zinc-500" title={subtitleText(item)}>
								{subtitleText(item)}
							</p>
						</a>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	<section class="mt-12">
		<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">Your libraries</h2>
		{#if loading}
			<p class="mt-6 text-zinc-400">Loading…</p>
		{:else if error}
			<p class="mt-6 font-mono text-rose-500">offline — {error}</p>
		{:else if libraries.length === 0}
			<div
				class="mt-6 rounded-lg border border-dashed border-zinc-300 p-8 text-center dark:border-zinc-700"
			>
				<p class="text-zinc-500">No libraries yet.</p>
				{#if auth.user?.is_admin}
					<a
						href={resolve('/admin/libraries')}
						class="mt-3 inline-block text-sm text-zinc-700 underline-offset-2 hover:underline dark:text-zinc-300"
					>
						Add one →
					</a>
				{:else}
					<p class="mt-2 text-sm text-zinc-400">Ask an administrator to add one.</p>
				{/if}
			</div>
		{:else}
			<ul class="mt-6 grid grid-cols-1 gap-4 sm:grid-cols-2">
				{#each libraries as library (library.id)}
					<li>
						<a
							href={resolve(`/library/${library.id}`)}
							class="block rounded-lg border border-zinc-200 p-5 transition hover:border-zinc-400 dark:border-zinc-800 dark:hover:border-zinc-600"
						>
							<p class="text-lg font-medium text-zinc-900 dark:text-zinc-100">
								{library.name}
							</p>
							<p class="mt-1 text-xs tracking-wide text-zinc-500 uppercase">
								{library.kind}
							</p>
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
</main>
