<script lang="ts">
	import './layout.css';
	import favicon from '$lib/assets/favicon.svg';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { auth, fetchAuthStatus } from '$lib/auth.svelte';

	let { children } = $props();

	/** Hide the SPA until we know whether the user is authenticated. */
	let booted = $state(false);

	onMount(async () => {
		await auth.refresh().catch(() => {
			// network down or 5xx — fall through and let the redirect logic land us
			// on /login, where the same call will be retried on submit.
		});

		const path = window.location.pathname;
		if (auth.user) {
			if (path === '/login' || path === '/setup') {
				await goto(resolve('/'));
			}
		} else {
			try {
				const status = await fetchAuthStatus();
				const target = status.bootstrapped ? '/login' : '/setup';
				if (path !== target) {
					await goto(status.bootstrapped ? resolve('/login') : resolve('/setup'));
				}
			} catch {
				// Can't tell whether bootstrap has happened; default to /login.
				if (path !== '/login') {
					await goto(resolve('/login'));
				}
			}
		}

		booted = true;
	});
</script>

<svelte:head><link rel="icon" href={favicon} /></svelte:head>

{#if booted}
	{@render children()}
{:else}
	<div class="flex h-screen items-center justify-center text-sm text-zinc-400">Loading…</div>
{/if}
