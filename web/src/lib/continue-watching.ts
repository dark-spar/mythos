import { apiGet } from './api';

interface ContinueWatchingMovie {
	kind: 'movie';
	id: string;
	library_id: string;
	title: string;
	year: number | null;
	poster_url: string | null;
	position_seconds: number;
	duration_seconds: number;
	updated_at: string;
}

interface ContinueWatchingEpisode {
	kind: 'episode';
	id: string;
	library_id: string;
	series_id: string;
	series_title: string;
	season_number: number;
	episode_number: number;
	episode_title: string | null;
	poster_url: string | null;
	still_url: string | null;
	position_seconds: number;
	duration_seconds: number;
	updated_at: string;
}

export type ContinueWatchingItem = ContinueWatchingMovie | ContinueWatchingEpisode;

export const listContinueWatching = (limit?: number): Promise<ContinueWatchingItem[]> => {
	const qs = limit != null ? `?limit=${limit}` : '';
	return apiGet(`/api/users/me/continue-watching${qs}`);
};

/// `Sxx Eyy` style label for an episode item.
export function episodeBadge(item: ContinueWatchingEpisode): string {
	const s = item.season_number.toString().padStart(2, '0');
	const e = item.episode_number.toString().padStart(2, '0');
	return `S${s}E${e}`;
}

/// Display title for a continue-watching item.
export function primaryTitle(item: ContinueWatchingItem): string {
	return item.kind === 'movie' ? item.title : item.series_title;
}

/// Subtitle line under the primary title.
export function subtitleText(item: ContinueWatchingItem): string {
	if (item.kind === 'movie') {
		return item.year != null ? String(item.year) : '';
	}
	const badge = episodeBadge(item);
	return item.episode_title ? `${badge} · ${item.episode_title}` : badge;
}

/// Target href to resume playback.
export function itemHref(item: ContinueWatchingItem): string {
	return item.kind === 'movie' ? `/movie/${item.id}` : `/episodes/${item.id}`;
}

/// 0..1 fraction. Clamps at boundaries so progress bars don't render
/// as 0 width or overflow.
export function progressFraction(item: ContinueWatchingItem): number {
	if (item.duration_seconds <= 0) return 0;
	const ratio = item.position_seconds / item.duration_seconds;
	if (!Number.isFinite(ratio)) return 0;
	return Math.min(1, Math.max(0, ratio));
}
