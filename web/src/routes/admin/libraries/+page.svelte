<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { auth } from '$lib/auth.svelte';
	import { ApiError } from '$lib/api';
	import {
		listLibraries,
		createLibrary,
		deleteLibrary,
		type Library,
		type LibraryKind
	} from '$lib/libraries';

	let libraries = $state<Library[]>([]);
	let loading = $state(true);
	let listError = $state<string | null>(null);

	let name = $state('');
	let kind = $state<LibraryKind>('movies');
	let rootPath = $state('');
	let submitting = $state(false);
	let formError = $state<string | null>(null);

	const KINDS: { value: LibraryKind; label: string }[] = [
		{ value: 'movies', label: 'Movies' },
		{ value: 'shows', label: 'TV Shows' },
		{ value: 'music', label: 'Music' },
		{ value: 'photos', label: 'Photos' },
		{ value: 'books', label: 'Books' }
	];

	onMount(async () => {
		if (!auth.user?.is_admin) {
			await goto(resolve('/'));
			return;
		}
		await load();
	});

	async function load() {
		loading = true;
		listError = null;
		try {
			libraries = await listLibraries();
		} catch (e) {
			listError = e instanceof Error ? e.message : 'failed to load libraries';
		} finally {
			loading = false;
		}
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		submitting = true;
		formError = null;
		try {
			await createLibrary({ name, kind, root_path: rootPath });
			name = '';
			rootPath = '';
			kind = 'movies';
			await load();
		} catch (e) {
			formError =
				e instanceof ApiError
					? errorMessage(e.code)
					: e instanceof Error
						? e.message
						: 'failed to create library';
		} finally {
			submitting = false;
		}
	}

	async function remove(library: Library) {
		if (!confirm(`Delete library "${library.name}"? Indexed contents will be removed.`)) {
			return;
		}
		try {
			await deleteLibrary(library.id);
			await load();
		} catch (e) {
			listError = e instanceof Error ? e.message : 'delete failed';
		}
	}

	function errorMessage(code: string): string {
		switch (code) {
			case 'name_required':
				return 'Name is required.';
			case 'root_path_not_absolute':
				return 'Root path must be absolute (start with "/").';
			case 'root_path_not_found':
				return 'That path does not exist on the server.';
			case 'root_path_not_directory':
				return 'That path exists but is not a directory.';
			case 'root_path_taken':
				return 'A library already points at that path.';
			case 'forbidden':
				return 'Admin permission required.';
			default:
				return code.replace(/_/g, ' ');
		}
	}
</script>

<svelte:head>
	<title>Libraries — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-3xl px-6 py-16">
	<header class="flex items-baseline justify-between">
		<h1 class="text-3xl font-semibold tracking-tight">Libraries</h1>
		<a
			href={resolve('/')}
			class="text-sm text-zinc-500 underline-offset-2 hover:text-zinc-900 hover:underline dark:hover:text-zinc-100"
		>
			← Home
		</a>
	</header>
	<p class="mt-2 text-sm text-zinc-500">
		Register root directories for Mythos to index. Scanning is implemented for the
		<span class="font-mono">movies</span> kind in this phase; other kinds can be created now and will
		be picked up when their scanners land.
	</p>

	<section class="mt-10 rounded-lg border border-zinc-200 p-5 dark:border-zinc-800">
		<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">Add a library</h2>
		<form onsubmit={submit} class="mt-4 space-y-3">
			<div class="grid grid-cols-1 gap-3 sm:grid-cols-[2fr_1fr]">
				<label class="block">
					<span class="text-xs text-zinc-500">Name</span>
					<input
						type="text"
						bind:value={name}
						required
						placeholder="Movies"
						class="mt-1 block w-full rounded-md border border-zinc-300 px-3 py-2 text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
					/>
				</label>
				<label class="block">
					<span class="text-xs text-zinc-500">Kind</span>
					<select
						bind:value={kind}
						class="mt-1 block w-full rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
					>
						{#each KINDS as k (k.value)}
							<option value={k.value}>{k.label}</option>
						{/each}
					</select>
				</label>
			</div>
			<label class="block">
				<span class="text-xs text-zinc-500">Root path on server</span>
				<input
					type="text"
					bind:value={rootPath}
					required
					placeholder="/var/media/movies"
					class="mt-1 block w-full rounded-md border border-zinc-300 px-3 py-2 font-mono text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
				/>
			</label>
			{#if formError}
				<p class="text-sm text-rose-500" role="alert">{formError}</p>
			{/if}
			<button
				type="submit"
				disabled={submitting}
				class="rounded-md bg-zinc-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-zinc-700 disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300"
			>
				{submitting ? 'Adding…' : 'Add library'}
			</button>
		</form>
	</section>

	<section class="mt-10">
		<h2 class="text-sm font-medium tracking-wide text-zinc-500 uppercase">Configured</h2>
		{#if loading}
			<p class="mt-4 text-zinc-400">Loading…</p>
		{:else if listError}
			<p class="mt-4 font-mono text-rose-500">offline — {listError}</p>
		{:else if libraries.length === 0}
			<p class="mt-4 text-zinc-400">None yet. Add one above.</p>
		{:else}
			<ul class="mt-4 divide-y divide-zinc-200 dark:divide-zinc-800">
				{#each libraries as library (library.id)}
					<li class="flex items-center justify-between py-3">
						<div class="min-w-0">
							<p class="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
								{library.name}
								<span
									class="ml-2 rounded bg-zinc-100 px-1.5 py-0.5 text-xs text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400"
								>
									{library.kind}
								</span>
							</p>
							<p class="mt-1 truncate font-mono text-xs text-zinc-500">{library.root_path}</p>
						</div>
						<button
							class="ml-4 text-sm text-rose-500 hover:text-rose-700"
							onclick={() => remove(library)}
						>
							Delete
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
</main>
