import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AUTOPLAY_COUNTDOWN_SECONDS, loadAutoPlay, saveAutoPlay } from './playback';

const KEY = 'mythos:autoplay-next';

// Minimal in-memory localStorage stub. Node's test env has no Web
// Storage by default; we only stub what these helpers actually call.
function freshStorageMock() {
	const store = new Map<string, string>();
	return {
		getItem: vi.fn((key: string) => store.get(key) ?? null),
		setItem: vi.fn((key: string, value: string) => {
			store.set(key, value);
		}),
		removeItem: vi.fn((key: string) => {
			store.delete(key);
		}),
		clear: vi.fn(() => store.clear()),
		key: vi.fn(() => null),
		get length() {
			return store.size;
		}
	};
}

beforeEach(() => {
	vi.stubGlobal('localStorage', freshStorageMock());
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('auto-play preference', () => {
	it('defaults to enabled when no value is stored', () => {
		expect(loadAutoPlay()).toBe(true);
	});

	it('round-trips true', () => {
		saveAutoPlay(true);
		expect(localStorage.getItem(KEY)).toBe('true');
		expect(loadAutoPlay()).toBe(true);
	});

	it('round-trips false', () => {
		saveAutoPlay(false);
		expect(localStorage.getItem(KEY)).toBe('false');
		expect(loadAutoPlay()).toBe(false);
	});

	it('returns enabled when localStorage.getItem throws', () => {
		const mock = freshStorageMock();
		mock.getItem.mockImplementation(() => {
			throw new Error('disabled');
		});
		vi.stubGlobal('localStorage', mock);
		expect(loadAutoPlay()).toBe(true);
	});

	it('save is a no-op when localStorage.setItem throws', () => {
		const mock = freshStorageMock();
		mock.setItem.mockImplementation(() => {
			throw new Error('quota');
		});
		vi.stubGlobal('localStorage', mock);
		expect(() => saveAutoPlay(false)).not.toThrow();
		expect(mock.setItem).toHaveBeenCalled();
	});

	it('exports a sensible countdown constant', () => {
		expect(AUTOPLAY_COUNTDOWN_SECONDS).toBeGreaterThan(0);
		expect(AUTOPLAY_COUNTDOWN_SECONDS).toBeLessThan(30);
	});
});
