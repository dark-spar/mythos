/**
 * Tiny fetch wrapper for authenticated JSON endpoints.
 *
 * On 401 with `{"error":"token_expired"}` the cached user is dropped and
 * the browser is redirected to `/login`. Other non-2xx responses throw
 * `ApiError` so callers can branch on the server-provided `code`.
 */

import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { auth } from './auth.svelte';

export class ApiError extends Error {
	constructor(
		public code: string,
		public status: number
	) {
		super(code);
		this.name = 'ApiError';
	}
}

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
	const init: RequestInit = {
		method,
		credentials: 'same-origin'
	};
	if (body !== undefined) {
		init.headers = { 'content-type': 'application/json' };
		init.body = JSON.stringify(body);
	}
	const res = await fetch(url, init);

	if (res.status === 401) {
		const json = (await res.json().catch(() => ({}))) as { error?: string };
		if (json.error === 'token_expired') {
			auth.user = null;
			await goto(resolve('/login'));
		}
		throw new ApiError(json.error ?? 'unauthorized', 401);
	}
	if (!res.ok) {
		const json = (await res.json().catch(() => ({}))) as { error?: string };
		throw new ApiError(json.error ?? `http_${res.status}`, res.status);
	}
	if (res.status === 204) return undefined as T;
	return (await res.json()) as T;
}

export const apiGet = <T>(url: string): Promise<T> => request<T>('GET', url);
export const apiPost = <T>(url: string, body?: unknown): Promise<T> =>
	request<T>('POST', url, body);
export const apiDelete = <T>(url: string): Promise<T> => request<T>('DELETE', url);
