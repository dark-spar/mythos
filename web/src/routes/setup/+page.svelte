<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { auth, AuthApiError } from '$lib/auth.svelte';

	let username = $state('');
	let password = $state('');
	let confirm = $state('');
	let error = $state<string | null>(null);
	let submitting = $state(false);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		if (password !== confirm) {
			error = 'Passwords do not match.';
			return;
		}
		if (password.length < 8) {
			error = 'Password must be at least 8 characters.';
			return;
		}
		submitting = true;
		try {
			await auth.register(username, password);
			await goto(resolve('/'));
		} catch (e) {
			error =
				e instanceof AuthApiError
					? errorMessage(e.code)
					: e instanceof Error
						? e.message
						: 'setup failed';
		} finally {
			submitting = false;
		}
	}

	function errorMessage(code: string): string {
		switch (code) {
			case 'forbidden':
				return 'Setup is already complete — sign in instead.';
			case 'invalid_credentials':
				return 'Username or password did not meet requirements.';
			default:
				return code.replace(/_/g, ' ');
		}
	}
</script>

<svelte:head>
	<title>Set up — Mythos</title>
</svelte:head>

<main class="mx-auto max-w-md px-6 py-24">
	<h1 class="text-3xl font-semibold tracking-tight">Welcome to Mythos</h1>
	<p class="mt-2 text-sm text-zinc-500">Create the first administrator account.</p>

	<form onsubmit={submit} class="mt-8 space-y-4">
		<label class="block">
			<span class="text-sm font-medium">Username</span>
			<input
				type="text"
				name="username"
				bind:value={username}
				autocomplete="username"
				required
				class="mt-1 block w-full rounded-md border border-zinc-300 px-3 py-2 text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
			/>
		</label>
		<label class="block">
			<span class="text-sm font-medium">Password</span>
			<input
				type="password"
				name="password"
				bind:value={password}
				autocomplete="new-password"
				required
				minlength="8"
				class="mt-1 block w-full rounded-md border border-zinc-300 px-3 py-2 text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
			/>
		</label>
		<label class="block">
			<span class="text-sm font-medium">Confirm password</span>
			<input
				type="password"
				name="confirm"
				bind:value={confirm}
				autocomplete="new-password"
				required
				class="mt-1 block w-full rounded-md border border-zinc-300 px-3 py-2 text-sm focus:border-zinc-900 focus:outline-none dark:border-zinc-700 dark:bg-zinc-900 dark:focus:border-zinc-100"
			/>
		</label>
		{#if error}
			<p class="text-sm text-rose-500" role="alert">{error}</p>
		{/if}
		<button
			type="submit"
			disabled={submitting}
			class="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white hover:bg-zinc-700 disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300"
		>
			{submitting ? 'Creating account…' : 'Create administrator'}
		</button>
	</form>
</main>
