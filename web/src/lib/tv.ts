import { apiGet } from './api';
import type { MediaFile, SubtitleTrack } from './movies';

export interface Series {
	id: string;
	library_id: string;
	title: string;
	sort_title: string;
	year: number | null;
	tmdb_id: number | null;
	overview: string | null;
	poster_url: string | null;
	created_at: string;
	updated_at: string;
}

export interface SeriesPage {
	items: Series[];
	total: number;
	limit: number;
	offset: number;
}

export interface Season {
	id: string;
	series_id: string;
	season_number: number;
	title: string | null;
	tmdb_id: number | null;
	overview: string | null;
	poster_url: string | null;
	created_at: string;
	updated_at: string;
}

export interface Episode {
	id: string;
	season_id: string;
	file_id: string;
	episode_number: number;
	title: string | null;
	tmdb_id: number | null;
	overview: string | null;
	still_url: string | null;
	air_date: string | null;
	created_at: string;
	updated_at: string;
}

export interface SeriesDetail {
	series: Series;
	seasons: Season[];
}

export interface SeasonDetail {
	series: Series;
	season: Season;
	episodes: Episode[];
}

export interface EpisodeProgress {
	position_seconds: number;
	duration_seconds: number;
	updated_at: string;
}

/// Lightweight episode reference returned for the prev/next slots on
/// EpisodeDetail. Crosses season boundaries, so `season_number` is
/// authoritative for the neighbor (do NOT borrow from the current
/// episode's season for the label).
export interface EpisodeNeighbor {
	id: string;
	season_number: number;
	episode_number: number;
	title: string | null;
}

export interface EpisodeDetail {
	episode: Episode;
	season: Season;
	series: Series;
	file: MediaFile;
	subtitles: SubtitleTrack[];
	prev: EpisodeNeighbor | null;
	next: EpisodeNeighbor | null;
	progress: EpisodeProgress | null;
}

export const listSeries = (
	libraryId: string,
	opts: { limit?: number; offset?: number } = {}
): Promise<SeriesPage> => {
	const params = new URLSearchParams();
	if (opts.limit != null) params.set('limit', String(opts.limit));
	if (opts.offset != null) params.set('offset', String(opts.offset));
	const qs = params.toString();
	return apiGet(`/api/libraries/${libraryId}/series${qs ? `?${qs}` : ''}`);
};

export const getSeries = (id: string): Promise<SeriesDetail> => apiGet(`/api/series/${id}`);

export const getSeason = (seriesId: string, seasonNumber: number): Promise<SeasonDetail> =>
	apiGet(`/api/series/${seriesId}/seasons/${seasonNumber}`);

export const getEpisode = (id: string): Promise<EpisodeDetail> => apiGet(`/api/episodes/${id}`);

export function seasonLabel(season: Season): string {
	if (season.season_number === 0) return season.title ?? 'Specials';
	return season.title ?? `Season ${season.season_number}`;
}
