<script lang="ts">
	import { onMount } from 'svelte';
	import Hls from 'hls.js';
	import { goto } from '$app/navigation';
	import { subtitleLabel, type MediaFile, type PlayResponse, type SubtitleTrack } from '../movies';
	import { clientProfile } from '../profile';
	import {
		AUTOPLAY_COUNTDOWN_SECONDS,
		hlsMasterUrl,
		loadAutoPlay,
		loadSavedSub,
		putItemProgress,
		requestItemPlay,
		saveAutoPlay,
		saveSub,
		sendItemProgressBeacon,
		stopTranscodeSession,
		type ItemKind
	} from './playback';

	type Props = {
		kind: ItemKind;
		itemId: string;
		file: MediaFile;
		subtitles: SubtitleTrack[];
		initialPositionSeconds?: number | null;
		/// Absolute href the overlay back-link points at. Movies use the
		/// library page; episodes use the series page.
		backHref: string;
		/// Optional "up next" target. When provided AND the user has
		/// auto-play enabled, the player shows a countdown card on the
		/// `ended` event and navigates to `href` when it elapses. The
		/// toggle and countdown card are hidden when this is omitted
		/// (e.g. on movie pages).
		next?: { href: string; label: string };
		/// When true, call `video.play()` once the stream attaches.
		/// The episode page sets this when it arrived via the
		/// auto-play countdown so consecutive episodes play seamlessly
		/// without preserving the surprising "every player page
		/// autoplays" default for manual navigation.
		autoplayOnMount?: boolean;
		/// Caller can react to the server-side playback decision once
		/// it's resolved (e.g., render a diagnostic banner outside the
		/// player). Optional.
		onPlanResolved?: (plan: PlayResponse) => void;
	};

	const {
		kind,
		itemId,
		file,
		subtitles,
		initialPositionSeconds,
		backHref,
		next,
		autoplayOnMount = false,
		onPlanResolved
	}: Props = $props();

	let videoEl: HTMLVideoElement | undefined = $state();
	let lastSavedAt = 0;
	let saveError = $state<string | null>(null);
	let hls: Hls | null = null;
	let playPlan = $state<PlayResponse | null>(null);
	let trackBlobUrl = $state<string | null>(null);
	let videoReady = $state(false);

	let selectedSubId = $state<string | null>(null);

	// Initialize from localStorage (and drop the saved choice if it
	// no longer matches a track in this file). Runs whenever kind /
	// itemId / subtitles change — i.e. on first mount and on any
	// navigation that reuses the component.
	$effect(() => {
		const saved = loadSavedSub(kind, itemId);
		if (saved && subtitles.some((s) => s.id === saved)) {
			selectedSubId = saved;
		} else {
			selectedSubId = null;
		}
	});

	const selectedSub = $derived<SubtitleTrack | null>(
		selectedSubId ? (subtitles.find((s) => s.id === selectedSubId) ?? null) : null
	);
	const imageSubId = $derived<string | null>(
		selectedSub && selectedSub.is_image ? selectedSub.id : null
	);
	const textSub = $derived<SubtitleTrack | null>(
		selectedSub && !selectedSub.is_image ? selectedSub : null
	);

	let backLinkVisible = $state(false);
	let backLinkIdleTimer: ReturnType<typeof setTimeout> | null = null;
	const BACK_LINK_IDLE_MS = 2000;
	function showBackLinkAwhile() {
		backLinkVisible = true;
		if (backLinkIdleTimer) clearTimeout(backLinkIdleTimer);
		backLinkIdleTimer = setTimeout(() => {
			backLinkVisible = false;
		}, BACK_LINK_IDLE_MS);
	}
	function hideBackLink() {
		if (backLinkIdleTimer) {
			clearTimeout(backLinkIdleTimer);
			backLinkIdleTimer = null;
		}
		backLinkVisible = false;
	}

	const SAVE_INTERVAL_MS = 10_000;

	// Auto-play-next state. The toggle is global to the browser
	// (stored under a single localStorage key) and only visible when
	// `next` is provided. The countdown is local to the current
	// component instance.
	let autoPlayEnabled = $state(true);
	let countdownActive = $state(false);
	let countdownRemaining = $state(AUTOPLAY_COUNTDOWN_SECONDS);
	let countdownTimer: ReturnType<typeof setInterval> | null = null;

	function toggleAutoPlay() {
		autoPlayEnabled = !autoPlayEnabled;
		saveAutoPlay(autoPlayEnabled);
		if (!autoPlayEnabled) cancelCountdown();
	}

	function startCountdown() {
		if (!next || !autoPlayEnabled || countdownActive) return;
		countdownActive = true;
		countdownRemaining = AUTOPLAY_COUNTDOWN_SECONDS;
		countdownTimer = setInterval(() => {
			countdownRemaining -= 1;
			if (countdownRemaining <= 0) {
				navigateNext();
			}
		}, 1000);
	}

	function cancelCountdown() {
		if (countdownTimer) {
			clearInterval(countdownTimer);
			countdownTimer = null;
		}
		countdownActive = false;
	}

	function navigateNext() {
		const href = next?.href;
		cancelCountdown();
		// `next.href` is composed by the caller from runtime ids
		// (`/episodes/{uuid}`), so SvelteKit's typed resolve() can't
		// type-check it; navigating with the dynamic string is the
		// intended behavior.
		//
		// Pass `state: { autoplay: true }` so the destination page
		// can tell this navigation came from the countdown card and
		// trigger playback automatically. Manual navigations leave
		// state unset, so visiting a player page by link / refresh
		// stays user-initiated.
		// eslint-disable-next-line svelte/no-navigation-without-resolve
		if (href) void goto(href, { state: { autoplay: true } });
	}

	function handleEnded() {
		startCountdown();
	}

	function handlePlaying() {
		// User resumed (seeked back, replayed end, etc.) — bail on the
		// queued navigation.
		if (countdownActive) cancelCountdown();
	}

	$effect(() => {
		if (!videoEl) return;
		const burnInSubId = imageSubId;
		const el = videoEl;
		let cancelled = false;
		(async () => {
			await attachPlayer(el, burnInSubId, () => cancelled);
		})();
		return () => {
			cancelled = true;
			cancelCountdown();
			detachPlayer();
		};
	});

	async function attachPlayer(
		el: HTMLVideoElement,
		burnInSubId: string | null,
		isCancelled: () => boolean
	) {
		let plan: PlayResponse;
		try {
			plan = await requestItemPlay(kind, itemId, clientProfile());
		} catch (e) {
			saveError = e instanceof Error ? `Couldn't plan playback: ${e.message}` : 'planning failed';
			return;
		}
		if (isCancelled()) return;
		playPlan = plan;
		onPlanResolved?.(plan);

		let url = plan.stream_url;
		if (burnInSubId) {
			// Burn-in requires the video encoder, so any mode that
			// keeps `-c:v copy` silently drops the overlay filter.
			// Force full re-encode regardless of what /play picked.
			url = hlsMasterUrl(kind, itemId, { mode: 'transcode_full', sub: burnInSubId });
		}

		const goingHls = plan.mode !== 'direct_play' || burnInSubId !== null;
		if (!goingHls) {
			el.src = url;
			tryAutoplay(el);
			return;
		}

		if (Hls.isSupported()) {
			const instance = new Hls({
				enableWorker: true,
				lowLatencyMode: false,
				// Seeks restart ffmpeg, which takes 2-4s to produce the
				// first segment of the new position. hls.js's defaults
				// (20s timeout, 6 retries, nudge-forward on stall) tear
				// through that startup budget and cascade into the
				// timeline-corruption death spiral. Give the server room
				// to actually deliver, and don't move the playhead while
				// waiting.
				fragLoadingTimeOut: 35000,
				fragLoadingMaxRetry: 2,
				fragLoadingRetryDelay: 2000,
				manifestLoadingTimeOut: 10000,
				levelLoadingTimeOut: 10000,
				nudgeMaxRetry: 0,
				highBufferWatchdogPeriod: 30
			});
			hls = instance;
			// MANIFEST_PARSED fires once the master + initial variant
			// are loaded; that's the right moment to start playback
			// rather than racing the manifest fetch.
			instance.once(Hls.Events.MANIFEST_PARSED, () => {
				tryAutoplay(el);
			});
			instance.loadSource(url);
			instance.attachMedia(el);
			return;
		}

		if (el.canPlayType('application/vnd.apple.mpegurl')) {
			el.src = url;
			tryAutoplay(el);
			return;
		}

		saveError = "Your browser has no HLS support — can't play this file.";
	}

	/// Attempt to start playback. Used when the caller signals that
	/// this mount came from an auto-play countdown (e.g. consecutive
	/// episode in a TV binge). The browser's autoplay policy may
	/// still block — there's no recent user gesture on a brand-new
	/// page load — so any rejection is swallowed silently and the
	/// user can press play manually.
	function tryAutoplay(el: HTMLVideoElement) {
		if (!autoplayOnMount) return;
		void el.play().catch(() => {
			// Autoplay blocked by browser policy or interrupted by a
			// fast unmount. Either way, controls remain visible.
		});
	}

	function detachPlayer() {
		if (hls) {
			hls.destroy();
			hls = null;
		}
		if (videoEl) {
			videoEl.removeAttribute('src');
			videoEl.load();
		}
		stopTranscodeSession(kind, itemId);
	}

	function handleLoadedMetadata() {
		if (
			videoEl &&
			initialPositionSeconds != null &&
			initialPositionSeconds > 1 &&
			!Number.isNaN(videoEl.duration)
		) {
			const target = Math.min(initialPositionSeconds, Math.max(0, videoEl.duration - 5));
			if (target > 0) {
				videoEl.currentTime = target;
			}
		}
	}

	function absolutePositionSeconds(): number | null {
		if (!videoEl || !Number.isFinite(videoEl.currentTime)) return null;
		return videoEl.currentTime;
	}

	function fullDurationSeconds(): number | null {
		if (file.duration_seconds && file.duration_seconds > 0) {
			return file.duration_seconds;
		}
		if (videoEl && Number.isFinite(videoEl.duration) && videoEl.duration > 0) {
			return videoEl.duration;
		}
		return null;
	}

	async function saveNow(reason: string) {
		const pos = absolutePositionSeconds();
		const dur = fullDurationSeconds();
		if (pos == null || dur == null || pos < 0 || dur <= 0) return;
		lastSavedAt = Date.now();
		try {
			await putItemProgress(kind, itemId, pos, dur);
			saveError = null;
		} catch (e) {
			saveError =
				e instanceof Error ? `Couldn't save progress (${reason}): ${e.message}` : 'save failed';
		}
	}

	function handleTimeUpdate() {
		if (Date.now() - lastSavedAt >= SAVE_INTERVAL_MS) {
			void saveNow('tick');
		}
	}

	function handlePause() {
		void saveNow('pause');
	}

	function handleVisibilityChange() {
		if (!videoEl) return;
		if (document.hidden && !videoEl.paused) {
			const pos = absolutePositionSeconds();
			const dur = fullDurationSeconds();
			if (pos != null && dur != null && pos >= 0 && dur > 0) {
				sendItemProgressBeacon(kind, itemId, pos, dur);
				lastSavedAt = Date.now();
			}
		}
	}

	function handleBeforeUnload() {
		if (!videoEl) return;
		const pos = absolutePositionSeconds();
		const dur = fullDurationSeconds();
		if (pos != null && dur != null && pos >= 0 && dur > 0) {
			sendItemProgressBeacon(kind, itemId, pos, dur);
		}
		stopTranscodeSession(kind, itemId);
	}

	function toggleFullscreen() {
		if (!videoEl) return;
		if (document.fullscreenElement) {
			void document.exitFullscreen();
		} else {
			void videoEl.requestFullscreen();
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (!videoEl) return;
		const target = event.target as HTMLElement | null;
		if (
			target &&
			(target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)
		) {
			return;
		}
		if (event.metaKey || event.ctrlKey || event.altKey) return;

		switch (event.key.toLowerCase()) {
			case 'f':
				event.preventDefault();
				toggleFullscreen();
				break;
			case ' ':
			case 'k':
				event.preventDefault();
				if (videoEl.paused) void videoEl.play();
				else videoEl.pause();
				break;
			case 'm':
				event.preventDefault();
				videoEl.muted = !videoEl.muted;
				break;
			case 'arrowright':
				event.preventDefault();
				videoEl.currentTime = Math.min(videoEl.duration || Infinity, videoEl.currentTime + 10);
				break;
			case 'arrowleft':
				event.preventDefault();
				videoEl.currentTime = Math.max(0, videoEl.currentTime - 10);
				break;
		}
	}

	function transcodeReasons(plan: PlayResponse): string[] {
		const d = plan.diagnostic;
		const out: string[] = [];
		if (!d.container_ok) out.push('container');
		if (!d.video_ok) out.push('video codec');
		if (!d.audio_ok) out.push('audio codec');
		if (!d.resolution_ok) out.push('resolution');
		return out;
	}

	function modeLabel(mode: PlayResponse['mode']): string {
		switch (mode) {
			case 'direct_play':
				return 'Direct play';
			case 'remux':
				return 'Remuxing (no re-encode)';
			case 'transcode_audio':
				return 'Re-encoding audio only';
			case 'transcode_video':
				return 'Re-encoding video only';
			case 'transcode_full':
				return 'Transcoding';
		}
	}

	function clearCuesOnDestroy(node: HTMLTrackElement) {
		return {
			destroy() {
				try {
					node.track.mode = 'disabled';
				} catch {
					// Video element already torn down — nothing to clear.
				}
			}
		};
	}

	$effect(() => {
		saveSub(kind, itemId, selectedSubId);
	});

	$effect(() => {
		const sub = textSub;
		if (!sub) {
			if (trackBlobUrl) URL.revokeObjectURL(trackBlobUrl);
			trackBlobUrl = null;
			return;
		}
		let cancelled = false;
		let created: string | null = null;
		fetch(`/api/${kind === 'movie' ? 'movies' : 'episodes'}/${itemId}/subtitles/${sub.id}/vtt`, {
			credentials: 'same-origin'
		})
			.then((r) => {
				if (!r.ok) throw new Error(`vtt ${r.status}`);
				return r.blob();
			})
			.then((blob) => {
				if (cancelled) return;
				created = URL.createObjectURL(blob);
				if (trackBlobUrl) URL.revokeObjectURL(trackBlobUrl);
				trackBlobUrl = created;
			})
			.catch((err) => {
				if (!cancelled) saveError = `Couldn't load subtitle: ${err.message}`;
			});
		return () => {
			cancelled = true;
			if (created) URL.revokeObjectURL(created);
		};
	});

	$effect(() => {
		const video = videoEl;
		if (!video) return;
		const onMetadata = () => {
			videoReady = true;
		};
		const onEmptied = () => {
			videoReady = false;
		};
		video.addEventListener('loadedmetadata', onMetadata);
		video.addEventListener('emptied', onEmptied);
		return () => {
			video.removeEventListener('loadedmetadata', onMetadata);
			video.removeEventListener('emptied', onEmptied);
			videoReady = false;
		};
	});

	$effect(() => {
		const video = videoEl;
		if (!video) return;
		void textSub;
		const apply = () => {
			for (let i = 0; i < video.textTracks.length; i++) {
				const t = video.textTracks[i];
				if (t.kind === 'subtitles' && t.mode !== 'showing') {
					t.mode = 'showing';
				}
			}
		};
		apply();
		video.textTracks.addEventListener('addtrack', apply);
		video.addEventListener('loadedmetadata', apply);
		return () => {
			video.textTracks.removeEventListener('addtrack', apply);
			video.removeEventListener('loadedmetadata', apply);
		};
	});

	const reasons = $derived(playPlan ? transcodeReasons(playPlan) : []);

	function formatDurationLocal(seconds: number): string {
		const total = Math.round(seconds);
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		const s = total % 60;
		if (h > 0) return `${h}h ${m.toString().padStart(2, '0')}m`;
		if (m > 0) return `${m}m ${s.toString().padStart(2, '0')}s`;
		return `${s}s`;
	}

	onMount(() => {
		autoPlayEnabled = loadAutoPlay();
		document.addEventListener('visibilitychange', handleVisibilityChange);
		window.addEventListener('beforeunload', handleBeforeUnload);
		window.addEventListener('keydown', handleKeydown);
		return () => {
			document.removeEventListener('visibilitychange', handleVisibilityChange);
			window.removeEventListener('beforeunload', handleBeforeUnload);
			window.removeEventListener('keydown', handleKeydown);
		};
	});
</script>

<section
	class="relative overflow-hidden bg-black"
	aria-label="Video player"
	onmousemove={showBackLinkAwhile}
	onmouseleave={hideBackLink}
>
	<!--
		`backHref` is a caller-supplied dynamic string (movie pages
		point at /library/<id>, episode pages at /series/<id>);
		SvelteKit's typed `resolve()` doesn't accept arbitrary path
		strings, so we render the href directly.
	-->
	<!-- eslint-disable svelte/no-navigation-without-resolve -->
	<a
		href={backHref}
		class="absolute top-4 left-4 z-10 rounded bg-black/60 px-3 py-1.5 text-sm text-zinc-100 backdrop-blur transition-opacity duration-200 hover:bg-black/80 {backLinkVisible
			? 'opacity-100'
			: 'pointer-events-none opacity-0'}"
	>
		← Back
	</a>
	<!-- eslint-enable svelte/no-navigation-without-resolve -->
	<video
		bind:this={videoEl}
		controls
		preload="metadata"
		class="mx-auto aspect-video max-h-[85vh] w-full"
		onloadedmetadata={handleLoadedMetadata}
		ontimeupdate={handleTimeUpdate}
		onpause={handlePause}
		onended={handleEnded}
		onplaying={handlePlaying}
	>
		{#if textSub && trackBlobUrl && videoReady}
			{#key trackBlobUrl}
				<track
					use:clearCuesOnDestroy
					kind="subtitles"
					src={trackBlobUrl}
					srclang={textSub.language ?? 'und'}
					label={subtitleLabel(textSub)}
					default
				/>
			{/key}
		{/if}
		Your browser can't play this file directly.
	</video>

	{#if next && countdownActive}
		<!--
			Up-next countdown card. Bottom-right overlay; positioned
			above the native controls so it remains tappable on touch.
			Clicking the card body navigates immediately; the
			Cancel button stays here on the current item.
		-->
		<!-- eslint-disable svelte/no-navigation-without-resolve -->
		<div
			class="pointer-events-auto absolute right-4 bottom-20 z-10 w-72 rounded-lg bg-black/80 p-4 text-sm text-zinc-100 shadow-lg backdrop-blur"
			role="dialog"
			aria-label="Up next"
		>
			<p class="text-xs tracking-wide text-zinc-400 uppercase">Up next</p>
			<p class="mt-1 truncate font-medium" title={next.label}>{next.label}</p>
			<p class="mt-2 text-xs text-zinc-400">
				Playing in {countdownRemaining}s
			</p>
			<div class="mt-3 flex justify-end gap-2">
				<button
					type="button"
					class="rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-200 hover:bg-zinc-800"
					onclick={cancelCountdown}
				>
					Cancel
				</button>
				<button
					type="button"
					class="rounded bg-rose-600 px-3 py-1 text-xs font-medium text-white hover:bg-rose-500"
					onclick={navigateNext}
				>
					Play now
				</button>
			</div>
		</div>
		<!-- eslint-enable svelte/no-navigation-without-resolve -->
	{/if}
</section>

<div class="mx-auto max-w-5xl px-6 pt-6">
	{#if subtitles.length > 0}
		<div class="mt-3 flex items-center gap-2 text-sm">
			<label for="subtitle-select" class="text-zinc-400">Subtitles</label>
			<select
				id="subtitle-select"
				bind:value={selectedSubId}
				class="rounded border border-zinc-700 bg-zinc-900 px-2 py-1 text-zinc-100"
			>
				<option value={null}>Off</option>
				{#each subtitles as sub (sub.id)}
					<option value={sub.id}>{subtitleLabel(sub)}</option>
				{/each}
			</select>
			{#if selectedSub?.is_image}
				<span class="text-xs text-zinc-400">— burning into stream</span>
			{/if}
		</div>
	{/if}
	{#if next}
		<label class="mt-3 flex cursor-pointer items-center gap-2 text-sm text-zinc-400">
			<input
				type="checkbox"
				checked={autoPlayEnabled}
				onchange={toggleAutoPlay}
				class="h-4 w-4 rounded border-zinc-600 bg-zinc-900 text-rose-500 focus:ring-rose-500"
			/>
			<span>Auto-play next episode</span>
		</label>
	{/if}
	{#if playPlan && playPlan.mode !== 'direct_play'}
		<div
			class="mt-2 rounded border border-sky-700 bg-sky-950 p-3 text-xs text-sky-200"
			role="status"
		>
			<p>
				<span class="font-medium">{modeLabel(playPlan.mode)}</span>
				{#if reasons.length > 0}
					— your browser doesn't natively support this file's
					<span class="font-mono">{reasons.join(' + ')}</span>.
				{/if}
			</p>
			{#if playPlan.mode === 'transcode_full' || playPlan.mode === 'transcode_video'}
				<p class="mt-2">
					First few seconds may take a moment as ffmpeg spins up. Big seeks restart the transcoder,
					so they pause briefly before resuming at the new spot.
				</p>
			{/if}
		</div>
	{/if}
	{#if saveError}
		<p class="mt-2 text-xs text-rose-400">{saveError}</p>
	{:else if initialPositionSeconds != null && initialPositionSeconds > 1}
		<p class="mt-2 text-xs text-zinc-400">
			Resuming from {formatDurationLocal(initialPositionSeconds)}. Press
			<kbd class="rounded border border-zinc-700 px-1">f</kbd> for fullscreen,
			<kbd class="rounded border border-zinc-700 px-1">space</kbd> to play/pause.
		</p>
	{:else}
		<p class="mt-2 text-xs text-zinc-400">
			<kbd class="rounded border border-zinc-700 px-1">f</kbd> fullscreen ·
			<kbd class="rounded border border-zinc-700 px-1">space</kbd> play/pause ·
			<kbd class="rounded border border-zinc-700 px-1">m</kbd> mute ·
			<kbd class="rounded border border-zinc-700 px-1">←/→</kbd> seek 10s
		</p>
	{/if}
</div>

<style>
	/*
	 * Browsers default to white-on-black-box for ::cue. Strip the
	 * box and replace contrast with a soft text-shadow so cues read
	 * as floating white text on top of the frame without the
	 * letterbox-looking backdrop.
	 */
	:global(video::cue) {
		background: transparent;
		background-color: rgba(0, 0, 0, 0);
		color: white;
		text-shadow:
			0 0 4px rgba(0, 0, 0, 0.9),
			0 1px 2px rgba(0, 0, 0, 0.95);
	}
</style>
