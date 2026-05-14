/**
 * Kind-aware playback URL + progress helpers used by `Player.svelte`.
 *
 * Both movie and episode pages mount the same player component; the
 * `ItemKind` discriminator threads through to URL construction and
 * progress endpoints so we don't have to fork the player for each
 * media type.
 */

import { apiGet, apiPost, apiPut } from '../api';
import type { ClientProfile, PlayResponse } from '../movies';

export type ItemKind = 'movie' | 'episode';

const KIND_BASE: Record<ItemKind, string> = {
	movie: '/api/movies',
	episode: '/api/episodes'
};

export function baseUrlFor(kind: ItemKind): string {
	return KIND_BASE[kind];
}

export function streamUrl(kind: ItemKind, itemId: string): string {
	return `${baseUrlFor(kind)}/${itemId}/stream`;
}

export function hlsMasterUrl(
	kind: ItemKind,
	itemId: string,
	opts: { mode?: string; sub?: string; v?: string } = {}
): string {
	const params = new URLSearchParams();
	if (opts.mode) params.set('mode', opts.mode);
	if (opts.sub) params.set('sub', opts.sub);
	if (opts.v) params.set('v', opts.v);
	const qs = params.toString();
	return `${baseUrlFor(kind)}/${itemId}/hls/master.m3u8${qs ? `?${qs}` : ''}`;
}

export function vttUrl(kind: ItemKind, itemId: string, subId: string): string {
	return `${baseUrlFor(kind)}/${itemId}/subtitles/${subId}/vtt`;
}

export function requestItemPlay(
	kind: ItemKind,
	itemId: string,
	profile: ClientProfile
): Promise<PlayResponse> {
	return apiPost(`${baseUrlFor(kind)}/${itemId}/play`, profile);
}

export function putItemProgress(
	kind: ItemKind,
	itemId: string,
	positionSeconds: number,
	durationSeconds: number
): Promise<void> {
	return apiPut(`${baseUrlFor(kind)}/${itemId}/progress`, {
		position_seconds: positionSeconds,
		duration_seconds: durationSeconds
	});
}

/**
 * Fire-and-forget progress write that survives page unload by using
 * `fetch` with `keepalive: true`.
 */
export function sendItemProgressBeacon(
	kind: ItemKind,
	itemId: string,
	positionSeconds: number,
	durationSeconds: number
): void {
	try {
		void fetch(`${baseUrlFor(kind)}/${itemId}/progress`, {
			method: 'PUT',
			credentials: 'same-origin',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				position_seconds: positionSeconds,
				duration_seconds: durationSeconds
			}),
			keepalive: true
		});
	} catch {
		// page going away — nothing actionable
	}
}

/**
 * DELETE the active HLS session for an item, marked `keepalive` so the
 * request survives page navigation / tab close.
 */
export function stopTranscodeSession(kind: ItemKind, itemId: string): void {
	try {
		void fetch(`${baseUrlFor(kind)}/${itemId}/hls`, {
			method: 'DELETE',
			credentials: 'same-origin',
			keepalive: true
		});
	} catch {
		// nothing actionable
	}
}

const SUB_STORAGE_PREFIX = 'mythos:sub';

function subStorageKey(kind: ItemKind, itemId: string): string {
	return `${SUB_STORAGE_PREFIX}:${kind}:${itemId}`;
}

export function loadSavedSub(kind: ItemKind, itemId: string): string | null {
	try {
		// Earlier movie versions wrote `mythos:sub:<id>` without the
		// kind segment. Honor that legacy key for movies so existing
		// users don't lose their subtitle choice across the upgrade.
		const direct = localStorage.getItem(subStorageKey(kind, itemId));
		if (direct) return direct;
		if (kind === 'movie') {
			return localStorage.getItem(`${SUB_STORAGE_PREFIX}:${itemId}`) || null;
		}
		return null;
	} catch {
		return null;
	}
}

export function saveSub(kind: ItemKind, itemId: string, subId: string | null): void {
	try {
		const key = subStorageKey(kind, itemId);
		if (subId) localStorage.setItem(key, subId);
		else localStorage.removeItem(key);
	} catch {
		// localStorage disabled — ignore
	}
}

export interface MovieDetail {
	movie: { id: string };
}

export interface BasicProgress {
	position_seconds: number;
	duration_seconds: number;
	updated_at: string;
}

export interface FileProbe {
	duration_seconds: number | null;
}

/** Look up a season+episode in friendly form for breadcrumbs. */
export function episodeLabel(seasonNumber: number, episodeNumber: number): string {
	const s = seasonNumber.toString().padStart(2, '0');
	const e = episodeNumber.toString().padStart(2, '0');
	return `S${s}E${e}`;
}

export const fetchSeasonDetail = (seriesId: string, seasonNumber: number) =>
	apiGet(`/api/series/${seriesId}/seasons/${seasonNumber}`);
