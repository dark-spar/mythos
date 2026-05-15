import { apiGet, apiPut } from './api';

export type TmdbSource = 'env' | 'db' | 'none';

export type TonemapAlgorithm = 'hable' | 'mobius' | 'reinhard' | 'bt2390';

export const TONEMAP_ALGORITHMS: readonly TonemapAlgorithm[] = [
	'hable',
	'mobius',
	'reinhard',
	'bt2390'
] as const;

export type TonemapPipeline = 'hardware' | 'software';

export const TONEMAP_PIPELINES: readonly TonemapPipeline[] = ['hardware', 'software'] as const;

export interface Settings {
	tmdb: {
		configured: boolean;
		source: TmdbSource;
		/// DB-stored value, returned so admins can copy / edit it.
		/// `null` when nothing is stored. The env-var value (if any)
		/// is intentionally never mirrored back here.
		value: string | null;
	};
	tonemap: {
		enabled: boolean;
		algorithm: TonemapAlgorithm;
		pipeline: TonemapPipeline;
		/// `false` when the server's ffmpeg doesn't have the GPU
		/// tonemap filter for the active encoder. The backend
		/// silently falls back to the Software pipeline; the UI
		/// uses this flag to surface why "Hardware" isn't taking
		/// effect.
		hardware_supported: boolean;
	};
}

export interface SettingsUpdate {
	tmdb_api_key?: string;
	tonemap_enabled?: boolean;
	tonemap_algorithm?: TonemapAlgorithm;
	tonemap_pipeline?: TonemapPipeline;
}

export const getSettings = (): Promise<Settings> => apiGet('/api/settings');

export const updateSettings = (body: SettingsUpdate): Promise<Settings> =>
	apiPut('/api/settings', body);
