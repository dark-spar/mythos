/**
 * Singleton auth store. Reactive via Svelte 5 runes so any component
 * can read `auth.user` and re-render when it changes.
 *
 * The JWT lives in an HttpOnly cookie that the browser handles itself.
 * The store never touches the token — it only tracks whether the
 * current session has an authenticated user by calling `/api/users/me`.
 *
 * `login` / `register` / `logout` are wrappers around the JSON API and
 * update `user` after the server confirms.
 */

import { goto } from '$app/navigation';
import { resolve } from '$app/paths';

export interface User {
	id: string;
	username: string;
	is_admin: boolean;
	created_at: string;
	updated_at: string;
}

export class AuthApiError extends Error {
	constructor(
		public code: string,
		public status: number
	) {
		super(code);
		this.name = 'AuthApiError';
	}
}

class AuthStore {
	user = $state<User | null>(null);

	async refresh(): Promise<void> {
		const res = await fetch('/api/users/me', { credentials: 'same-origin' });
		if (res.status === 401) {
			this.user = null;
			return;
		}
		if (!res.ok) {
			throw new Error(`HTTP ${res.status}`);
		}
		this.user = (await res.json()) as User;
	}

	async login(username: string, password: string): Promise<void> {
		await this.#post('/api/auth/login', { username, password });
		await this.refresh();
	}

	async register(username: string, password: string): Promise<void> {
		await this.#post('/api/auth/register', { username, password });
		await this.refresh();
	}

	async logout(): Promise<void> {
		try {
			await fetch('/api/auth/logout', {
				method: 'POST',
				credentials: 'same-origin'
			});
		} catch {
			// Network down or server gone — drop the local session anyway.
		}
		this.user = null;
		await goto(resolve('/login'));
	}

	async #post(url: string, body: unknown): Promise<Response> {
		const res = await fetch(url, {
			method: 'POST',
			credentials: 'same-origin',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
		if (!res.ok) {
			const json = (await res.json().catch(() => ({}))) as { error?: string };
			throw new AuthApiError(json.error ?? 'unauthorized', res.status);
		}
		return res;
	}
}

export const auth = new AuthStore();

export async function fetchAuthStatus(): Promise<{ bootstrapped: boolean }> {
	const res = await fetch('/api/auth/status', { credentials: 'same-origin' });
	if (!res.ok) throw new Error(`HTTP ${res.status}`);
	return (await res.json()) as { bootstrapped: boolean };
}
