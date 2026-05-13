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

export const listLibraries = (): Promise<Library[]> => apiGet('/api/libraries');

export const createLibrary = (input: NewLibrary): Promise<Library> =>
	apiPost('/api/libraries', input);

export const deleteLibrary = (id: string): Promise<void> => apiDelete(`/api/libraries/${id}`);
