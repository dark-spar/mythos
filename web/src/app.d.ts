// See https://svelte.dev/docs/kit/types#app.d.ts
// for information about these interfaces
declare global {
	namespace App {
		// interface Error {}
		// interface Locals {}
		// interface PageData {}
		interface PageState {
			/// Set by the player when the auto-play countdown navigates
			/// to the next episode. The destination page reads this
			/// and tells the Player to call `video.play()` after the
			/// stream attaches. Manual navigations don't set it, so
			/// arriving at a player page by clicking a link or
			/// refreshing keeps the existing "user-initiated playback
			/// only" behavior.
			autoplay?: boolean;
		}
		// interface Platform {}
	}
}

export {};
