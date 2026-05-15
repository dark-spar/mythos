import { apiGet, apiPut } from './api';

export type TmdbSource = 'env' | 'db' | 'none';

export type TonemapAlgorithm = 'hable' | 'mobius' | 'reinhard' | 'bt2390';

export const TONEMAP_ALGORITHMS: readonly TonemapAlgorithm[] = [
	'hable',
	'mobius',
	'reinhard',
	'bt2390'
] as const;

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
	};
}

export interface SettingsUpdate {
	tmdb_api_key?: string;
	tonemap_enabled?: boolean;
	tonemap_algorithm?: TonemapAlgorithm;
}

export const getSettings = (): Promise<Settings> => apiGet('/api/settings');

export const updateSettings = (body: SettingsUpdate): Promise<Settings> =>
	apiPut('/api/settings', body);
