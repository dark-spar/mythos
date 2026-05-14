import type { ClientProfile, VideoCodecCap, AudioCodecCap } from './movies';

/// Probe the running browser for codec/container support and turn
/// it into a `ClientProfile` the server can match against the
/// source file. Uses both `MediaSource.isTypeSupported` (what
/// hls.js will hit via MSE) and `HTMLMediaElement.canPlayType`
/// (what direct-play hits) — anything either says yes to counts.
///
/// We deliberately keep the matrix small. Phase 6 will widen it via
/// the Jellyfin shim; until then, false negatives just push playback
/// onto the full transcode path, which is the safe default.
export function probeClientProfile(): ClientProfile {
	const video = document.createElement('video');

	const can = (mime: string): boolean => {
		if (canPlayMse(mime)) return true;
		const verdict = video.canPlayType(mime);
		return verdict === 'probably' || verdict === 'maybe';
	};

	const video_codecs: VideoCodecCap[] = [];
	// H.264 levels we probe at; level 41 (1080p60) is the practical
	// ceiling we ever transcode to.
	for (const [codecsAttr, profile, level] of [
		['avc1.4d401f', 'main', 31], // 720p Main
		['avc1.4d4028', 'main', 40], // 1080p Main
		['avc1.4d4029', 'main', 41], // 1080p60 Main
		['avc1.640028', 'high', 40], // 1080p High
		['avc1.640029', 'high', 41] // 1080p60 High
	] as const) {
		if (can(`video/mp4; codecs="${codecsAttr}"`)) {
			video_codecs.push({ codec: 'h264', profile, level });
		}
	}
	if (can('video/mp4; codecs="hvc1.1.6.L120.90"')) {
		video_codecs.push({ codec: 'hevc', profile: 'main', level: 120 });
	}
	if (can('video/mp4; codecs="hvc1.2.4.L120.90"')) {
		video_codecs.push({ codec: 'hevc', profile: 'main10', level: 120 });
	}
	if (can('video/webm; codecs="vp9"') || can('video/mp4; codecs="vp09.00.10.08"')) {
		video_codecs.push({ codec: 'vp9', profile: null, level: null });
	}
	if (can('video/mp4; codecs="av01.0.04M.08"')) {
		video_codecs.push({ codec: 'av1', profile: null, level: null });
	}

	const audio_codecs: AudioCodecCap[] = [];
	if (can('audio/mp4; codecs="mp4a.40.2"')) {
		audio_codecs.push({ codec: 'aac', max_channels: 8 });
	}
	if (can('audio/mp4; codecs="ac-3"')) {
		audio_codecs.push({ codec: 'ac3', max_channels: 6 });
	}
	if (can('audio/mp4; codecs="ec-3"')) {
		audio_codecs.push({ codec: 'eac3', max_channels: 8 });
	}
	if (can('audio/mp4; codecs="opus"') || can('audio/webm; codecs="opus"')) {
		audio_codecs.push({ codec: 'opus', max_channels: 8 });
	}
	if (can('audio/mpeg')) {
		audio_codecs.push({ codec: 'mp3', max_channels: 2 });
	}

	// Containers. mp4 + webm are universal on modern browsers. mkv,
	// avi, ts, mov — browsers don't demux these natively, even when
	// codecs are supported. The user-agent guess for the screen height
	// is good enough; we don't try to detect viewport size since users
	// commonly play on a small window on a 4K monitor.
	const containers = ['mp4'];
	if (video.canPlayType('video/webm; codecs="vp9"')) containers.push('webm');

	const screenHeight = window.screen?.height ?? 1080;
	const max_height = approxMaxHeight(screenHeight);
	const max_width = (max_height * 16) / 9;

	return {
		containers,
		video_codecs,
		audio_codecs,
		max_width: Math.round(max_width),
		max_height,
		max_audio_channels: 8
	};
}

function canPlayMse(mime: string): boolean {
	if (typeof MediaSource === 'undefined') return false;
	try {
		return MediaSource.isTypeSupported(mime);
	} catch {
		return false;
	}
}

/// Round the device's screen height up to the nearest ABR rung
/// (480/720/1080). Capped at 1080 — that's the highest tier the
/// server emits today. Conservative rounding keeps a 768-line laptop
/// in the 720p tier rather than chewing through an unnecessary
/// 1080p re-encode.
function approxMaxHeight(screenH: number): number {
	if (screenH <= 480) return 480;
	if (screenH <= 720) return 720;
	return 1080;
}

/// Probe once per page load. Capability doesn't change while the tab
/// is alive, so a cached profile is fine and lets repeated playback
/// requests share the same handshake.
let cached: ClientProfile | null = null;
export function clientProfile(): ClientProfile {
	if (!cached) cached = probeClientProfile();
	return cached;
}
