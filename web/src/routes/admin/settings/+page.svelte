<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { auth } from '$lib/auth.svelte';
	import { getSettings, updateSettings, type Settings } from '$lib/settings';

	let settings = $state<Settings | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	let tmdbInput = $state('');
	let tmdbVisible = $state(false);
	let saving = $state(false);
	let saveError = $state<string | null>(null);
	let saveOk = $state(false);

	onMount(async () => {
		if (!auth.user?.is_admin) {
			await goto(resolve('/'));
			return;
		}
		await load();
	});

	async function load() {
		loading = true;
		loadError = null;
		try {
			settings = await getSettings();
			tmdbInput = settings.tmdb.value ?? '';
		} catch (e) {
			loadError = e instanceof Error ? e.message : 'failed to load';
		} finally {
			loading = false;
		}
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		saving = true;
		saveError = null;
		saveOk = false;
		try {
			settings = await updateSettings({ tmdb_api_key: tmdbInput });
			tmdbInput = settings.tmdb.value ?? '';
			saveOk = true;
		} catch (e) {
			saveError = e instanceof Error ? e.message : 'failed to save';
		} finally {
			saving = false;
		}
	}

	async function clearTmdb() {
		if (!confirm('Clear the stored TMDb API key?')) return;
		saving = true;
		saveError = null;
		saveOk = false;
		try {
			settings = await updateSettings({ tmdb_api_key: '' });
			tmdbInput = settings.tmdb.value ?? '';
			saveOk = true;
		} catch (e) {
			saveError = e instanceof Error ? e.message : 'failed to clear';
		} finally {
			saving = false;
		}
	}

	function statusLabel(s: Settings['tmdb']): string {
		if (s.source === 'env') return 'Configured (set via MYTHOS_TMDB_API_KEY)';
		if (s.source === 'db') return 'Configured';
		return 'Not configured';
	}
</script>

<svelte:head>
	<title>Settings — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-3xl px-6 py-12">
	<a
		href={resolve('/')}
		class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
	>
		← Home
	</a>

	<h1 class="mt-4 text-3xl font-semibold tracking-tight">Settings</h1>

	{#if loading}
		<p class="mt-8 text-zinc-400">Loading…</p>
	{:else if loadError}
		<p class="mt-8 font-mono text-rose-500">{loadError}</p>
	{:else if settings}
		<section class="mt-8 rounded-lg border border-zinc-200 p-6 dark:border-zinc-800">
			<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">TMDb API key</h2>
			<p class="mt-2 text-sm text-zinc-600 dark:text-zinc-400">
				Used by the scanner to enrich movies with descriptions and posters. Get a key at
				<a
					href="https://www.themoviedb.org/settings/api"
					target="_blank"
					rel="noopener"
					class="underline">themoviedb.org</a
				>.
			</p>

			<dl class="mt-4 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
				<dt class="text-zinc-500">Status</dt>
				<dd class="text-zinc-900 dark:text-zinc-100">{statusLabel(settings.tmdb)}</dd>
			</dl>

			{#if settings.tmdb.source === 'env'}
				<div
					class="mt-4 rounded border border-amber-300 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-700 dark:bg-amber-950 dark:text-amber-200"
				>
					The environment variable <code class="font-mono">MYTHOS_TMDB_API_KEY</code> is set and takes
					precedence. Anything you save here will be stored in the database but won't take effect until
					the env var is unset and the server restarts.
				</div>
			{/if}

			<form onsubmit={save} class="mt-6 flex flex-col gap-3">
				<label for="tmdb-key" class="text-sm font-medium text-zinc-700 dark:text-zinc-300">
					{settings.tmdb.source === 'db' ? 'Replace key' : 'Set key'}
				</label>
				<div class="flex items-stretch gap-2">
					<input
						id="tmdb-key"
						type={tmdbVisible ? 'text' : 'password'}
						autocomplete="off"
						placeholder="v3 hex key, or v4 read-access JWT"
						bind:value={tmdbInput}
						class="flex-1 rounded border border-zinc-300 bg-white px-3 py-2 font-mono text-sm text-zinc-900 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100"
					/>
					<button
						type="button"
						onclick={() => (tmdbVisible = !tmdbVisible)}
						aria-pressed={tmdbVisible}
						class="rounded border border-zinc-300 bg-white px-3 text-xs text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800"
					>
						{tmdbVisible ? 'Hide' : 'Show'}
					</button>
				</div>
				<div class="flex items-center gap-3">
					<button
						type="submit"
						disabled={saving || tmdbInput.trim().length === 0}
						class="rounded bg-zinc-900 px-4 py-2 text-sm text-white disabled:cursor-not-allowed disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
					>
						{saving ? 'Saving…' : 'Save'}
					</button>
					{#if settings.tmdb.source === 'db'}
						<button
							type="button"
							onclick={clearTmdb}
							disabled={saving}
							class="text-sm text-rose-600 underline-offset-2 hover:underline disabled:opacity-50 dark:text-rose-400"
						>
							Clear stored key
						</button>
					{/if}
				</div>
				{#if saveError}
					<p class="text-xs text-rose-500">{saveError}</p>
				{:else if saveOk}
					<p class="text-xs text-emerald-600 dark:text-emerald-400">
						Saved. Next library scan will use the new key.
					</p>
				{/if}
			</form>
		</section>
	{/if}
</main>
