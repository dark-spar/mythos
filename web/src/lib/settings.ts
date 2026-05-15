import { apiGet, apiPut } from './api';

export type TmdbSource = 'env' | 'db' | 'none';

export type TonemapAlgorithm = 'hable' | 'mobius' | 'reinhard' | 'bt2390';

export const TONEMAP_ALGORITHMS: readonly TonemapAlgorithm[] = [
	'hable',
	'mobius',
	'reinhard',
	'bt2390'
] as const;

export type TonemapPipeline = 'software' | 'vaapi' | 'opencl' | 'cuda';

export type Encoder = 'cpu' | 'qsv' | 'vaapi' | 'nvenc' | 'videotoolbox';

export interface PipelineOption {
	value: TonemapPipeline;
	/// `false` when the named filter isn't compiled into the
	/// server's ffmpeg. The radio button is rendered disabled with
	/// an "(unavailable)" hint so the operator knows why a GPU
	/// option is greyed out.
	available: boolean;
}

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
		/// Active encoder slug — drives which pipelines we render.
		encoder: Encoder;
		/// Pipelines valid for `encoder`, paired with build
		/// availability. Always at least `software`.
		pipeline_options: PipelineOption[];
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
