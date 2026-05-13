import { apiGet, apiPut } from './api';

export interface Probe {
	container: string | null;
	video_codec: string | null;
	audio_codec: string | null;
	duration_seconds: number | null;
	width: number | null;
	height: number | null;
}

export interface MediaFile {
	id: string;
	library_id: string;
	path: string;
	size_bytes: number;
	mtime: string;
	scanned_at: string;
	container: string | null;
	video_codec: string | null;
	audio_codec: string | null;
	duration_seconds: number | null;
	width: number | null;
	height: number | null;
}

export interface Movie {
	id: string;
	library_id: string;
	file_id: string;
	title: string;
	sort_title: string;
	year: number | null;
	tmdb_id: number | null;
	overview: string | null;
	poster_url: string | null;
	created_at: string;
	updated_at: string;
}

export interface MoviesPage {
	items: Movie[];
	total: number;
	limit: number;
	offset: number;
}

export interface WatchProgress {
	position_seconds: number;
	duration_seconds: number;
	updated_at: string;
}

export interface MovieDetail {
	movie: Movie;
	file: MediaFile;
	progress: WatchProgress | null;
}

export const putProgress = (
	movieId: string,
	positionSeconds: number,
	durationSeconds: number
): Promise<void> =>
	apiPut(`/api/movies/${movieId}/progress`, {
		position_seconds: positionSeconds,
		duration_seconds: durationSeconds
	});

/// Fire-and-forget progress write that survives page unload by using
/// `fetch` with `keepalive: true`. Returns no useful value — used in
/// `beforeunload` / `visibilitychange` handlers where the browser won't
/// wait for a full round-trip.
export function sendProgressBeacon(
	movieId: string,
	positionSeconds: number,
	durationSeconds: number
): void {
	try {
		void fetch(`/api/movies/${movieId}/progress`, {
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
		// nothing useful to do — page is going away
	}
}

export const listMovies = (
	libraryId: string,
	opts: { limit?: number; offset?: number } = {}
): Promise<MoviesPage> => {
	const params = new URLSearchParams();
	if (opts.limit != null) params.set('limit', String(opts.limit));
	if (opts.offset != null) params.set('offset', String(opts.offset));
	const qs = params.toString();
	return apiGet(`/api/libraries/${libraryId}/movies${qs ? `?${qs}` : ''}`);
};

export const getMovie = (id: string): Promise<MovieDetail> => apiGet(`/api/movies/${id}`);

export function formatDuration(seconds: number | null): string {
	if (seconds == null) return '—';
	const total = Math.round(seconds);
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = total % 60;
	if (h > 0) return `${h}h ${m.toString().padStart(2, '0')}m`;
	if (m > 0) return `${m}m ${s.toString().padStart(2, '0')}s`;
	return `${s}s`;
}

export function formatResolution(file: MediaFile): string | null {
	if (file.width != null && file.height != null) {
		return `${file.width}×${file.height}`;
	}
	return null;
}

export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let n = bytes / 1024;
	let unit = units[0];
	for (let i = 1; i < units.length && n >= 1024; i++) {
		n /= 1024;
		unit = units[i];
	}
	return `${n.toFixed(1)} ${unit}`;
}

/// Browser-friendly codec/container allowlists. Best effort — actual
/// support varies by browser and codec build flags. We surface a warning
/// when something falls outside these lists so users aren't left guessing
/// why a track is silent.
const BROWSER_SAFE_VIDEO = ['h264', 'avc1', 'vp9', 'vp09', 'av1', 'av01'];
const BROWSER_SAFE_AUDIO = ['aac', 'mp4a', 'mp3', 'opus', 'vorbis'];
const BROWSER_UNSAFE_EXT = ['mkv', 'avi', 'wmv', 'ts', 'm2ts', 'mov'];

export interface CompatIssue {
	kind: 'container' | 'video_codec' | 'audio_codec';
	value: string;
}

export function browserCompatIssues(file: MediaFile): CompatIssue[] {
	const issues: CompatIssue[] = [];
	const ext = file.path.split('.').pop()?.toLowerCase();
	if (ext && BROWSER_UNSAFE_EXT.includes(ext)) {
		issues.push({ kind: 'container', value: ext });
	}
	if (file.video_codec && !BROWSER_SAFE_VIDEO.includes(file.video_codec.toLowerCase())) {
		issues.push({ kind: 'video_codec', value: file.video_codec });
	}
	if (file.audio_codec && !BROWSER_SAFE_AUDIO.includes(file.audio_codec.toLowerCase())) {
		issues.push({ kind: 'audio_codec', value: file.audio_codec });
	}
	return issues;
}
