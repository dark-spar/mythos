<script lang="ts">
	import { resolve } from '$app/paths';
	import { auth } from '$lib/auth.svelte';

	let health = $state<{ status: string; version: string } | null>(null);
	let error = $state<string | null>(null);

	async function checkHealth() {
		error = null;
		try {
			const res = await fetch('/api/health');
			if (!res.ok) throw new Error(`HTTP ${res.status}`);
			health = await res.json();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			health = null;
		}
	}

	$effect(() => {
		checkHealth();
	});
</script>

<svelte:head>
	<title>Mythos</title>
</svelte:head>

<main class="mx-auto max-w-2xl px-6 py-16">
	<header class="flex items-baseline justify-between">
		<div>
			<h1 class="text-4xl font-semibold tracking-tight">Mythos</h1>
			<p class="mt-3 text-zinc-500">A self-hosted media server, written in Rust.</p>
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

	<section class="mt-10 rounded-lg border border-zinc-200 p-5 dark:border-zinc-800">
		<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">Server</h2>
		{#if health}
			<p class="mt-2 text-zinc-900 dark:text-zinc-100">
				<span class="inline-block size-2 rounded-full bg-emerald-500"></span>
				<span class="ml-2 font-mono">{health.status}</span>
				<span class="ml-2 text-zinc-400">v{health.version}</span>
			</p>
		{:else if error}
			<p class="mt-2 font-mono text-rose-500">offline — {error}</p>
		{:else}
			<p class="mt-2 text-zinc-400">checking…</p>
		{/if}
		<button
			class="mt-4 rounded-md bg-zinc-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-zinc-700 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300"
			onclick={checkHealth}
		>
			Refresh
		</button>
	</section>
</main>
