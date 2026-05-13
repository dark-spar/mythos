import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/navigation', () => ({
	goto: vi.fn(async () => {})
}));

vi.mock('$app/paths', () => ({
	resolve: (p: string) => p
}));

import { AuthApiError, auth, fetchAuthStatus } from './auth.svelte';
import { goto } from '$app/navigation';

const fakeUser = {
	id: '00000000-0000-7000-8000-000000000000',
	username: 'admin',
	is_admin: true,
	created_at: '2026-01-01T00:00:00.000Z',
	updated_at: '2026-01-01T00:00:00.000Z'
};

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

beforeEach(() => {
	auth.user = null;
	vi.restoreAllMocks();
});

describe('auth store', () => {
	it('refresh populates user on 200', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(jsonResponse(fakeUser));
		await auth.refresh();
		expect(auth.user).toEqual(fakeUser);
	});

	it('refresh clears user on 401', async () => {
		auth.user = fakeUser;
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(null, { status: 401 }));
		await auth.refresh();
		expect(auth.user).toBeNull();
	});

	it('refresh throws on 5xx', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(null, { status: 500 }));
		await expect(auth.refresh()).rejects.toThrow(/HTTP 500/);
	});

	it('login posts then refreshes user', async () => {
		const fetchSpy = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValueOnce(jsonResponse({ token: 't', user: fakeUser }))
			.mockResolvedValueOnce(jsonResponse(fakeUser));

		await auth.login('admin', 'hunter2hunter2');

		expect(fetchSpy).toHaveBeenNthCalledWith(
			1,
			'/api/auth/login',
			expect.objectContaining({ method: 'POST', credentials: 'same-origin' })
		);
		expect(fetchSpy).toHaveBeenNthCalledWith(
			2,
			'/api/users/me',
			expect.objectContaining({ credentials: 'same-origin' })
		);
		expect(auth.user).toEqual(fakeUser);
	});

	it('login throws AuthApiError when credentials are wrong', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(
			jsonResponse({ error: 'invalid_credentials' }, 401)
		);
		await expect(auth.login('admin', 'nope')).rejects.toBeInstanceOf(AuthApiError);
		expect(auth.user).toBeNull();
	});

	it('register works the same as login', async () => {
		const fetchSpy = vi
			.spyOn(globalThis, 'fetch')
			.mockResolvedValueOnce(jsonResponse({ token: 't', user: fakeUser }))
			.mockResolvedValueOnce(jsonResponse(fakeUser));
		await auth.register('admin', 'hunter2hunter2');
		expect(fetchSpy).toHaveBeenNthCalledWith(
			1,
			'/api/auth/register',
			expect.objectContaining({ method: 'POST' })
		);
		expect(auth.user).toEqual(fakeUser);
	});

	it('logout clears user and navigates to /login', async () => {
		auth.user = fakeUser;
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(null, { status: 204 }));

		await auth.logout();

		expect(auth.user).toBeNull();
		expect(goto).toHaveBeenCalledWith('/login');
	});

	it('logout still clears user when the server call fails', async () => {
		auth.user = fakeUser;
		vi.spyOn(globalThis, 'fetch').mockRejectedValueOnce(new Error('network'));
		await auth.logout();
		expect(auth.user).toBeNull();
		expect(goto).toHaveBeenCalledWith('/login');
	});
});

describe('fetchAuthStatus', () => {
	it('returns the parsed status payload', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(jsonResponse({ bootstrapped: true }));
		await expect(fetchAuthStatus()).resolves.toEqual({ bootstrapped: true });
	});
});
