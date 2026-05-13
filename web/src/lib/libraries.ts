import { apiDelete, apiGet, apiPost } from './api';

export type LibraryKind = 'movies' | 'shows' | 'music' | 'photos' | 'books';

export interface Library {
	id: string;
	name: string;
	kind: LibraryKind;
	root_path: string;
	created_at: string;
	updated_at: string;
}

export interface NewLibrary {
	name: string;
	kind: LibraryKind;
	root_path: string;
}

export type ScanState =
	| { state: 'idle' }
	| { state: 'running'; started_at: string }
	| {
			state: 'completed';
			started_at: string;
			finished_at: string;
			added: number;
			updated: number;
			removed: number;
			errors: string[];
			duration_ms: number;
	  };

export const listLibraries = (): Promise<Library[]> => apiGet('/api/libraries');

export const getLibrary = (id: string): Promise<Library> => apiGet(`/api/libraries/${id}`);

export const createLibrary = (input: NewLibrary): Promise<Library> =>
	apiPost('/api/libraries', input);

export const deleteLibrary = (id: string): Promise<void> => apiDelete(`/api/libraries/${id}`);

export const startScan = (id: string): Promise<ScanState> => apiPost(`/api/libraries/${id}/scan`);

export const getScanState = (id: string): Promise<ScanState> => apiGet(`/api/libraries/${id}/scan`);
