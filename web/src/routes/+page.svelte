<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { auth } from '$lib/auth.svelte';
	import { listLibraries, type Library } from '$lib/libraries';

	let libraries = $state<Library[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			libraries = await listLibraries();
		} catch (e) {
			error = e instanceof Error ? e.message : 'failed to load libraries';
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
