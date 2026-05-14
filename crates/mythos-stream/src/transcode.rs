//! Live HLS transcoding session manager.
//!
//! One ffmpeg subprocess per `(user_id, movie_id)` key. Sessions are
//! identified by their `start_segment`: calling
//! [`TranscodeManager::ensure_session_for_segment`] with a segment
//! that's within the current session's window reuses the running
//! ffmpeg; a request before the session start, or far past its
//! frontier, kills it and starts a new one (seek-by-restart).
//!
//! Each session writes locally-numbered segments (`seg-0.ts`,
//! `seg-1.ts`, …) into its own work dir. The HTTP layer maps the
//! client's global `seg-N.ts` request to the session's local index by
//! subtracting `start_segment`.
//!
//! Sessions reap after [`IDLE_TIMEOUT`] of no segment-request
//! activity. The reaper is owned by the server binary — call
//! [`TranscodeManager::reap_idle`] from a periodic task.
//!
//! `kill_on_drop(true)` on the underlying [`tokio::process::Child`]
//! means dropping a session (manager dropped, process exit, etc.) sends
//! `SIGKILL` to ffmpeg. Combined with explicit kills on seek-restart,
//! no zombie ffmpegs should linger.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info};
use uuid::Uuid;

use crate::hwaccel::HwAccel;

/// Target HLS segment duration in seconds. Anything that builds the
/// synthetic playlist must use this same constant or the playlist's
/// `#EXTINF` durations won't line up with what ffmpeg actually
/// produces.
pub const SEGMENT_DURATION_SECS: f64 = 6.0;

/// How long a session can go without a segment request before the
/// reaper kills its ffmpeg and removes its on-disk work.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a segment-fetch waits for ffmpeg to produce the requested
/// segment before giving up. Sequential playback rarely waits at all;
/// this exists for first-segment startup and for the case where the
/// player skips ahead by a few segments and ffmpeg has to catch up.
pub const SEGMENT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of segments past the current frontier we'll let a
/// reuse linger before forcing a restart. Routine forward playback
/// keeps the player within 1–2 segments of the frontier (ffmpeg
/// outpaces realtime), so this only fires on real seeks.
const MAX_AHEAD_SEGMENTS: u32 = 30;

/// Don't kill a session this young — it's still booting ffmpeg and
/// would barely produce a segment before being torn down again. A
/// segment request that arrives during this window for an incompatible
/// offset gets a "too early, try again in a bit" error rather than
/// triggering an immediate restart, which protects against rogue
/// clients (a forgotten browser tab, an extension fetching across the
/// timeline) stampeding ffmpeg into never producing anything.
const RESTART_GRACE_PERIOD: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum TranscodeError {
    #[error("ffmpeg failed to start: {0}")]
    Spawn(std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffmpeg never produced the requested segment within the timeout")]
    Timeout,
    #[error("invalid filename for segment: {0}")]
    InvalidFilename(String),
    #[error("requested segment {requested} is before the session start ({session_start})")]
    BeforeSessionStart { requested: u32, session_start: u32 },
    #[error("a session was just started for this key; try again shortly")]
    SessionStillBooting,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SessionKey {
    pub user_id: Uuid,
    pub movie_id: Uuid,
}

pub struct TranscodeSession {
    pub key: SessionKey,
    /// Global segment index this session's `seg-0.ts` represents.
    pub start_segment: u32,
    pub work_dir: PathBuf,
    pub started_at: Instant,
    last_access: Mutex<Instant>,
    child: Mutex<Option<Child>>,
}

impl TranscodeSession {
    /// Filesystem path for the global segment `seg_idx`. Returns
    /// `BeforeSessionStart` if the requested segment predates the
    /// session — the caller is expected to restart the session in that
    /// case rather than reaching for a negative local index.
    pub fn local_segment_path(&self, seg_idx: u32) -> Result<PathBuf, TranscodeError> {
        if seg_idx < self.start_segment {
            return Err(TranscodeError::BeforeSessionStart {
                requested: seg_idx,
                session_start: self.start_segment,
            });
        }
        let local = seg_idx - self.start_segment;
        Ok(self.work_dir.join(format!("seg-{local}.ts")))
    }

    /// Highest global segment index this session has produced so far,
    /// or `None` if it hasn't produced anything yet.
    pub async fn frontier(&self) -> Option<u32> {
        let mut max_local: Option<u32> = None;
        let mut entries = match tokio::fs::read_dir(&self.work_dir).await {
            Ok(e) => e,
            Err(_) => return None,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(name) = entry.file_name().to_str()
                && let Some(rest) = name
                    .strip_prefix("seg-")
                    .and_then(|r| r.strip_suffix(".ts"))
                && let Ok(n) = rest.parse::<u32>()
            {
                max_local = Some(max_local.map_or(n, |m| m.max(n)));
            }
        }
        max_local.map(|local| self.start_segment + local)
    }

    pub async fn touch(&self) {
        *self.last_access.lock().await = Instant::now();
    }

    async fn kill(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            // Give ffmpeg up to 2s to flush, then move on.
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

#[derive(Clone)]
pub struct TranscodeManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    work_root: PathBuf,
    sessions: RwLock<HashMap<SessionKey, Arc<TranscodeSession>>>,
    accel: HwAccel,
}

impl TranscodeManager {
    pub fn new(work_root: PathBuf, accel: HwAccel) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                work_root,
                sessions: RwLock::new(HashMap::new()),
                accel,
            }),
        }
    }

    pub fn hwaccel(&self) -> HwAccel {
        self.inner.accel
    }

    /// Return a session that's either currently transcoding segment
    /// `seg_idx` or will be very shortly. Restarts the session if the
    /// requested segment is before its start or too far past its
    /// frontier; reuses otherwise.
    ///
    /// If the existing session is younger than
    /// [`RESTART_GRACE_PERIOD`] and the requested segment isn't
    /// compatible with it, returns [`TranscodeError::SessionStillBooting`]
    /// rather than killing the in-flight ffmpeg. The HTTP layer turns
    /// that into a 503 + `Retry-After`, which protects the manager
    /// from clients (forgotten tabs, video extensions, etc.) issuing
    /// rapid segment requests across the timeline.
    pub async fn ensure_session_for_segment(
        &self,
        key: SessionKey,
        input_path: &Path,
        seg_idx: u32,
    ) -> Result<Arc<TranscodeSession>, TranscodeError> {
        // Fast path: existing session covers this segment.
        let needs_restart = {
            let sessions = self.inner.sessions.read().await;
            match sessions.get(&key) {
                Some(existing) if seg_idx >= existing.start_segment => {
                    existing.touch().await;
                    let frontier = existing.frontier().await;
                    let too_far_ahead = match frontier {
                        Some(f) => seg_idx > f + MAX_AHEAD_SEGMENTS,
                        // No segments yet — only restart if the player
                        // is asking for something far past start.
                        None => seg_idx > existing.start_segment + MAX_AHEAD_SEGMENTS,
                    };
                    if too_far_ahead {
                        if existing.started_at.elapsed() < RESTART_GRACE_PERIOD {
                            return Err(TranscodeError::SessionStillBooting);
                        }
                        true
                    } else {
                        return Ok(existing.clone());
                    }
                }
                Some(existing) => {
                    // seg_idx < start_segment — user seeked back.
                    if existing.started_at.elapsed() < RESTART_GRACE_PERIOD {
                        return Err(TranscodeError::SessionStillBooting);
                    }
                    true
                }
                None => true,
            }
        };
        if !needs_restart {
            // Logically unreachable, but spell it out for clarity.
            return Err(TranscodeError::Timeout);
        }
        self.restart_at(key, input_path, seg_idx).await
    }

    /// Start a fresh session at `seg_idx`, killing any existing session
    /// under the same key.
    async fn restart_at(
        &self,
        key: SessionKey,
        input_path: &Path,
        seg_idx: u32,
    ) -> Result<Arc<TranscodeSession>, TranscodeError> {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(existing) = sessions.remove(&key) {
            debug!(
                user = %existing.key.user_id,
                movie = %existing.key.movie_id,
                old_start = existing.start_segment,
                new_start = seg_idx,
                "restarting transcode session"
            );
            existing.kill().await;
            let _ = tokio::fs::remove_dir_all(&existing.work_dir).await;
        }

        let work_dir = self
            .inner
            .work_root
            .join(key.user_id.to_string())
            .join(key.movie_id.to_string());
        tokio::fs::create_dir_all(&work_dir).await?;

        let offset_seconds = f64::from(seg_idx) * SEGMENT_DURATION_SECS;
        info!(
            user = %key.user_id,
            movie = %key.movie_id,
            seg_idx,
            offset_seconds,
            "starting ffmpeg transcode session"
        );

        let child = launch_ffmpeg(input_path, &work_dir, offset_seconds, self.inner.accel)
            .await
            .map_err(TranscodeError::Spawn)?;

        let session = Arc::new(TranscodeSession {
            key: key.clone(),
            start_segment: seg_idx,
            work_dir,
            started_at: Instant::now(),
            last_access: Mutex::new(Instant::now()),
            child: Mutex::new(Some(child)),
        });
        sessions.insert(key, session.clone());
        Ok(session)
    }

    pub async fn get(&self, key: &SessionKey) -> Option<Arc<TranscodeSession>> {
        self.inner.sessions.read().await.get(key).cloned()
    }

    pub async fn stop(&self, key: &SessionKey) {
        let removed = self.inner.sessions.write().await.remove(key);
        if let Some(session) = removed {
            session.kill().await;
            let _ = tokio::fs::remove_dir_all(&session.work_dir).await;
        }
    }

    /// Kill and remove sessions whose `last_access` is older than
    /// [`IDLE_TIMEOUT`]. Returns the count reaped.
    pub async fn reap_idle(&self) -> usize {
        let cutoff = Instant::now()
            .checked_sub(IDLE_TIMEOUT)
            .unwrap_or_else(Instant::now);
        let to_kill: Vec<Arc<TranscodeSession>> = {
            let mut sessions = self.inner.sessions.write().await;
            let mut victims = Vec::new();
            let keys: Vec<SessionKey> = sessions.keys().cloned().collect();
            for k in keys {
                let stale = match sessions.get(&k) {
                    Some(s) => *s.last_access.lock().await < cutoff,
                    None => false,
                };
                if stale && let Some(removed) = sessions.remove(&k) {
                    victims.push(removed);
                }
            }
            victims
        };
        let count = to_kill.len();
        for session in to_kill {
            info!(
                user = %session.key.user_id,
                movie = %session.key.movie_id,
                "reaping idle transcode session"
            );
            session.kill().await;
            let _ = tokio::fs::remove_dir_all(&session.work_dir).await;
        }
        count
    }

    pub async fn session_count(&self) -> usize {
        self.inner.sessions.read().await.len()
    }
}

/// Wait for `path` to exist, polling every 100ms up to `timeout`.
pub async fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), TranscodeError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(TranscodeError::Timeout)
}

async fn launch_ffmpeg(
    input: &Path,
    work_dir: &Path,
    offset_seconds: f64,
    accel: HwAccel,
) -> std::io::Result<Child> {
    let playlist = work_dir.join("playlist.m3u8");
    let segment_pattern = work_dir.join("seg-%d.ts");

    // -ss before -i is fast (uses container index, may be slightly
    // imprecise). For HLS the segment boundary doesn't have to be
    // frame-exact — the player adjusts. Fast seek keeps startup
    // latency low.
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner").arg("-loglevel").arg("warning");
    // HW decode flags go before -i. Encode-only accelerators (e.g.
    // VideoToolbox at the encoder side without a matching hwaccel)
    // return an empty slice here and decode falls back to CPU.
    cmd.args(accel.decode_args());
    cmd.arg("-ss")
        .arg(format!("{offset_seconds:.3}"))
        .arg("-accurate_seek")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a:0?")
        // -copyts (+ muxdelay/muxpreload 0) preserves the source's
        // wall-clock PTS through the encoder, so segment N really
        // starts at N * SEGMENT_DURATION_SECS in the player's
        // timeline rather than at PTS 0. Without this, hls.js sees
        // segments whose internal PTS resets to 0 each session and
        // gradually decides the movie is shorter than it actually
        // is — which is exactly the "timeline shrinks, skip to end"
        // failure mode after a seek triggers a session restart.
        .arg("-copyts")
        .arg("-muxdelay")
        .arg("0")
        .arg("-muxpreload")
        .arg("0")
        // Force keyframes at every segment boundary on the *encoder*
        // side so each segment is independently decodable regardless
        // of the source's keyframe layout. (independent_segments on
        // the HLS muxer alone isn't enough when the source's GOPs
        // don't align to our 6s grid.)
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{SEGMENT_DURATION_SECS})"));
    cmd.args(accel.encode_args());
    cmd.arg("-c:a")
        .arg("aac")
        .arg("-ac")
        .arg("2")
        .arg("-b:a")
        .arg("128k")
        .arg("-f")
        .arg("hls")
        .arg("-hls_time")
        .arg(format!("{SEGMENT_DURATION_SECS}"))
        .arg("-hls_list_size")
        .arg("0")
        .arg("-hls_flags")
        .arg("independent_segments")
        .arg("-hls_segment_filename")
        .arg(&segment_pattern)
        .arg(&playlist)
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Some(stderr) = child.stderr.take() {
        // ffmpeg's stderr at -loglevel warning is mostly informational
        // ("Consider increasing analyzeduration…" and similar). Real
        // failures surface as a non-zero exit or as our segment-wait
        // timeout, not as a specific stderr line — log at info so
        // these don't look like actionable problems.
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                info!(source = "ffmpeg", "{line}");
            }
        });
    }
    Ok(child)
}

/// Parse `seg-N.ts` into `N`. Anything else (including
/// `playlist.m3u8`) returns `None`.
pub fn parse_segment_filename(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg-")?.strip_suffix(".ts")?;
    rest.parse::<u32>().ok()
}

/// Build a static VOD playlist describing every segment up to
/// `total_duration_seconds`. The last segment may be shorter than
/// [`SEGMENT_DURATION_SECS`] to absorb the leftover.
pub fn build_vod_playlist(total_duration_seconds: f64) -> String {
    let total = total_duration_seconds.max(0.0);
    if total <= 0.0 {
        return "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-ENDLIST\n".to_string();
    }
    let seg_dur = SEGMENT_DURATION_SECS;
    let n_full = (total / seg_dur).floor() as u32;
    let leftover = total - f64::from(n_full) * seg_dur;
    let n_segments = if leftover > 0.001 { n_full + 1 } else { n_full };

    let mut p = String::with_capacity(64 + (n_segments as usize) * 32);
    p.push_str("#EXTM3U\n");
    p.push_str("#EXT-X-VERSION:6\n");
    p.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        seg_dur.ceil() as u32
    ));
    p.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
    p.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    p.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");

    for i in 0..n_segments {
        let dur = if i == n_full && leftover > 0.001 {
            leftover
        } else {
            seg_dur
        };
        p.push_str(&format!("#EXTINF:{dur:.3},\n"));
        p.push_str(&format!("seg-{i}.ts\n"));
    }
    p.push_str("#EXT-X-ENDLIST\n");
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_segment_filename_accepts_well_formed_names() {
        assert_eq!(parse_segment_filename("seg-0.ts"), Some(0));
        assert_eq!(parse_segment_filename("seg-12345.ts"), Some(12345));
        assert_eq!(parse_segment_filename("seg-.ts"), None);
        assert_eq!(parse_segment_filename("playlist.m3u8"), None);
        assert_eq!(parse_segment_filename("../etc/passwd"), None);
        assert_eq!(parse_segment_filename("seg-12.tsX"), None);
    }

    #[test]
    fn vod_playlist_lists_every_segment_with_correct_durations() {
        let p = build_vod_playlist(13.0); // 6 + 6 + 1
        assert!(p.contains("seg-0.ts"));
        assert!(p.contains("seg-1.ts"));
        assert!(p.contains("seg-2.ts"));
        assert!(!p.contains("seg-3.ts"));
        assert!(p.contains("#EXTINF:6.000"));
        assert!(p.contains("#EXTINF:1.000"));
        assert!(p.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn vod_playlist_with_exact_multiple_has_no_short_tail() {
        let p = build_vod_playlist(12.0);
        assert!(p.contains("seg-0.ts"));
        assert!(p.contains("seg-1.ts"));
        assert!(!p.contains("seg-2.ts"));
    }

    #[test]
    fn vod_playlist_with_zero_duration_is_empty() {
        let p = build_vod_playlist(0.0);
        assert!(!p.contains("seg-"));
        assert!(p.contains("#EXT-X-ENDLIST"));
    }
}
