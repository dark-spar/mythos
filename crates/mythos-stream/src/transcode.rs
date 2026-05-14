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

use crate::abr::{ABR_LADDER, Rendition, is_known_variant};
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
    #[error("unknown ABR variant: {0}")]
    InvalidVariant(String),
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
    /// Absolute ffprobe stream index of the subtitle being burned in
    /// for this session, or `None` if subs are off. A request with a
    /// different value forces a restart, same as a backward seek.
    pub burn_in_sub: Option<i64>,
    pub work_dir: PathBuf,
    pub started_at: Instant,
    last_access: Mutex<Instant>,
    child: Mutex<Option<Child>>,
}

impl TranscodeSession {
    /// Filesystem path for the global segment `seg_idx` within the
    /// named variant. Returns `BeforeSessionStart` if the requested
    /// segment predates the session (caller should restart), or
    /// `InvalidVariant` for an unknown variant name.
    pub fn local_segment_path(
        &self,
        variant: &str,
        seg_idx: u32,
    ) -> Result<PathBuf, TranscodeError> {
        if !is_known_variant(variant) {
            return Err(TranscodeError::InvalidVariant(variant.to_string()));
        }
        if seg_idx < self.start_segment {
            return Err(TranscodeError::BeforeSessionStart {
                requested: seg_idx,
                session_start: self.start_segment,
            });
        }
        let local = seg_idx - self.start_segment;
        Ok(self.work_dir.join(variant).join(format!("seg-{local}.ts")))
    }

    /// Filesystem path for the variant-specific playlist ffmpeg writes
    /// alongside the segments. Not currently served to clients — the
    /// SPA gets a synthetic playlist via [`build_variant_playlist`] —
    /// but useful for diagnostics.
    pub fn variant_playlist_path(&self, variant: &str) -> Result<PathBuf, TranscodeError> {
        if !is_known_variant(variant) {
            return Err(TranscodeError::InvalidVariant(variant.to_string()));
        }
        Ok(self.work_dir.join(variant).join("playlist.m3u8"))
    }

    /// Highest global segment index produced for `variant`, or `None`
    /// if the variant hasn't written its first segment yet. All
    /// renditions are encoded in lockstep by a single ffmpeg, so any
    /// variant's frontier approximates the session's overall
    /// progress; we read the specific variant being asked about to
    /// avoid races on subdirs that haven't been created yet.
    pub async fn frontier(&self, variant: &str) -> Option<u32> {
        if !is_known_variant(variant) {
            return None;
        }
        let dir = self.work_dir.join(variant);
        let mut max_local: Option<u32> = None;
        let mut entries = match tokio::fs::read_dir(&dir).await {
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
    /// requested segment is before its start, too far past its
    /// frontier, or has a different `burn_in_sub` selection; reuses
    /// otherwise.
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
        variant: &str,
        seg_idx: u32,
        burn_in_sub: Option<i64>,
    ) -> Result<Arc<TranscodeSession>, TranscodeError> {
        if !is_known_variant(variant) {
            return Err(TranscodeError::InvalidVariant(variant.to_string()));
        }
        // Fast path: existing session covers this segment AND has the
        // same subtitle selection.
        let needs_restart = {
            let sessions = self.inner.sessions.read().await;
            match sessions.get(&key) {
                Some(existing) if existing.burn_in_sub != burn_in_sub => {
                    if existing.started_at.elapsed() < RESTART_GRACE_PERIOD {
                        return Err(TranscodeError::SessionStillBooting);
                    }
                    true
                }
                Some(existing) if seg_idx >= existing.start_segment => {
                    existing.touch().await;
                    let frontier = existing.frontier(variant).await;
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
        self.restart_at(key, input_path, seg_idx, burn_in_sub).await
    }

    /// Start a fresh session at `seg_idx`, killing any existing session
    /// under the same key.
    async fn restart_at(
        &self,
        key: SessionKey,
        input_path: &Path,
        seg_idx: u32,
        burn_in_sub: Option<i64>,
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
        // ffmpeg's HLS muxer doesn't auto-create per-variant subdirs
        // when `%v` is in `-hls_segment_filename`; pre-create them.
        for rendition in ABR_LADDER {
            tokio::fs::create_dir_all(work_dir.join(rendition.name)).await?;
        }

        let offset_seconds = f64::from(seg_idx) * SEGMENT_DURATION_SECS;
        // Force the CPU pipeline when burning in image subs. The
        // `overlay` filter on VAAPI-resident surfaces is brittle
        // (driver/libva version sensitive) and the perf hit only
        // shows when a user explicitly turns subs on.
        let session_accel = match burn_in_sub {
            Some(_) => HwAccel::Cpu,
            None => self.inner.accel,
        };
        info!(
            user = %key.user_id,
            movie = %key.movie_id,
            seg_idx,
            offset_seconds,
            encoder = session_accel.h264_encoder(),
            renditions = ABR_LADDER.len(),
            burn_in_sub = ?burn_in_sub,
            "starting ffmpeg transcode session"
        );

        let child = launch_ffmpeg(
            input_path,
            &work_dir,
            offset_seconds,
            session_accel,
            burn_in_sub,
        )
        .await
        .map_err(TranscodeError::Spawn)?;

        let session = Arc::new(TranscodeSession {
            key: key.clone(),
            start_segment: seg_idx,
            burn_in_sub,
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
    burn_in_sub: Option<i64>,
) -> std::io::Result<Child> {
    // Per-variant subdirs are pre-created by the caller. ffmpeg
    // expands `%v` in `-hls_segment_filename` and the output URL to
    // the variant name set in `-var_stream_map`.
    let segment_pattern = work_dir.join("%v").join("seg-%d.ts");
    let variant_playlist_pattern = work_dir.join("%v").join("playlist.m3u8");

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner").arg("-loglevel").arg("warning");
    cmd.args(accel.decode_args());
    // PGS subtitle events frequently lack a duration ("show until
    // next event"); -fix_sub_duration fills those in so overlay
    // displays them for the right amount of time.
    if burn_in_sub.is_some() {
        cmd.arg("-fix_sub_duration");
    }
    cmd.arg("-ss")
        .arg(format!("{offset_seconds:.3}"))
        .arg("-accurate_seek")
        .arg("-i")
        .arg(input);

    // Single decode → optional sub burn-in → split N ways → per-
    // rendition scale. With VAAPI the split + scale all happen on the
    // GPU; with libx264 they all happen on the CPU. Either way, one
    // decode pass. When burn_in_sub is set the caller has already
    // forced accel to Cpu.
    cmd.arg("-filter_complex")
        .arg(build_filter_complex(accel, burn_in_sub));

    // Map: each rendition gets the corresponding scaled video output
    // [vN] + a copy of the source audio.
    for i in 0..ABR_LADDER.len() {
        cmd.arg("-map").arg(format!("[v{i}]"));
        cmd.arg("-map").arg("0:a:0?");
    }

    // Per-rendition video encoder args (codec, bitrate target,
    // maxrate, bufsize, preset where applicable).
    for (i, rendition) in ABR_LADDER.iter().enumerate() {
        cmd.args(accel.abr_video_encoder_args(i, rendition));
    }

    // Audio: AAC stereo, per-variant bitrate.
    cmd.arg("-c:a").arg("aac").arg("-ac").arg("2");
    for (i, rendition) in ABR_LADDER.iter().enumerate() {
        cmd.arg(format!("-b:a:{i}"))
            .arg(format!("{}k", rendition.audio_bitrate_kbps));
    }

    // PTS preservation + segment-boundary keyframes (see Phase 4
    // commentary in git history for why each of these matters).
    cmd.arg("-copyts")
        .arg("-muxdelay")
        .arg("0")
        .arg("-muxpreload")
        .arg("0")
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{SEGMENT_DURATION_SECS})"));

    // HLS muxer: write the per-variant playlists into `%v/playlist.m3u8`
    // and segments into `%v/seg-%d.ts`. We don't actually serve the
    // master that ffmpeg writes; the API builds a synthetic one. But
    // ffmpeg still needs `-master_pl_name` set to opt into the
    // var_stream_map mode.
    cmd.arg("-f")
        .arg("hls")
        .arg("-hls_time")
        .arg(format!("{SEGMENT_DURATION_SECS}"))
        .arg("-hls_list_size")
        .arg("0")
        .arg("-hls_flags")
        .arg("independent_segments")
        .arg("-var_stream_map")
        .arg(build_var_stream_map())
        .arg("-master_pl_name")
        .arg("master.m3u8")
        .arg("-hls_segment_filename")
        .arg(&segment_pattern)
        .arg(&variant_playlist_pattern)
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                info!(source = "ffmpeg", "{line}");
            }
        });
    }
    Ok(child)
}

/// Construct the `-filter_complex` argument that optionally burns a
/// subtitle stream into the video, then splits the result N ways and
/// scales each branch to one rendition.
fn build_filter_complex(accel: HwAccel, burn_in_sub: Option<i64>) -> String {
    let n = ABR_LADDER.len();

    // With burn-in: overlay first, then split the composited frame.
    // Without burn-in: split the raw video stream directly. The label
    // we feed into split is `[base]` either way.
    let mut graph = String::new();
    let split_input = match burn_in_sub {
        Some(stream_index) => {
            // ffmpeg auto-converts PGS/VOBSUB bitmap streams into a
            // video-overlay-compatible source when used as overlay's
            // second input. We use absolute stream indexing so it's
            // unambiguous even with multiple subtitle tracks.
            graph.push_str(&format!("[0:v][0:{stream_index}]overlay[base];"));
            "[base]"
        }
        None => "[0:v]",
    };

    // <input>split=3[s0][s1][s2]
    let split_labels: String = (0..n).map(|i| format!("[s{i}]")).collect();
    graph.push_str(&format!("{split_input}split={n}{split_labels}"));

    // ; [s0]scale=...[v0]; [s1]scale=...[v1]; ...
    for (i, rendition) in ABR_LADDER.iter().enumerate() {
        graph.push(';');
        graph.push_str(&format!("[s{i}]{}[v{i}]", accel.scale_filter(rendition)));
    }
    graph
}

/// `-var_stream_map` value: pairs output indices to variant names.
/// With 3 video outputs and 3 audio outputs, video stream `i` and
/// audio stream `i` belong to variant `ABR_LADDER[i].name`.
fn build_var_stream_map() -> String {
    ABR_LADDER
        .iter()
        .enumerate()
        .map(|(i, r)| format!("v:{i},a:{i},name:{}", r.name))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse `seg-N.ts` into `N`. Anything else (including
/// `playlist.m3u8`) returns `None`.
pub fn parse_segment_filename(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg-")?.strip_suffix(".ts")?;
    rest.parse::<u32>().ok()
}

/// Synthetic ABR master playlist. Lists every rendition in
/// [`ABR_LADDER`] with its declared bandwidth + resolution +
/// codecs hint. Relative URLs point at `:variant/playlist.m3u8`
/// served by the API; the player fetches whichever it picks.
///
/// `query` is appended verbatim to each variant URL (e.g.
/// `"?sub=<uuid>"`), so a sub selection from the master URL
/// propagates to the variant playlist requests. Relative URL
/// resolution in HLS clients drops the query otherwise.
pub fn build_master_playlist(query: &str) -> String {
    let mut p = String::new();
    p.push_str("#EXTM3U\n");
    p.push_str("#EXT-X-VERSION:6\n");
    p.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    for r in ABR_LADDER {
        p.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{},CODECS=\"{}\"\n",
            r.declared_bandwidth_bps(),
            r.width,
            r.height,
            r.codecs_attr()
        ));
        p.push_str(&format!("{}/playlist.m3u8{query}\n", r.name));
    }
    p
}

/// Synthetic per-variant media playlist describing every segment up
/// to `total_duration_seconds`. The last segment may be shorter than
/// [`SEGMENT_DURATION_SECS`] to absorb the leftover. Segment URLs are
/// `seg-N.ts` relative to the playlist itself (which is served at
/// `:variant/playlist.m3u8`).
///
/// `query` is appended verbatim to each segment URL so a sub
/// selection from the playlist URL propagates to segment requests.
pub fn build_variant_playlist(
    total_duration_seconds: f64,
    variant: &Rendition,
    query: &str,
) -> String {
    let _ = variant; // signature reserves room for per-variant
    // bitrate metadata once we want it
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
        p.push_str(&format!("seg-{i}.ts{query}\n"));
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
    fn variant_playlist_lists_every_segment_with_correct_durations() {
        let p = build_variant_playlist(13.0, &ABR_LADDER[0], ""); // 6 + 6 + 1
        assert!(p.contains("seg-0.ts"));
        assert!(p.contains("seg-1.ts"));
        assert!(p.contains("seg-2.ts"));
        assert!(!p.contains("seg-3.ts"));
        assert!(p.contains("#EXTINF:6.000"));
        assert!(p.contains("#EXTINF:1.000"));
        assert!(p.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn variant_playlist_with_exact_multiple_has_no_short_tail() {
        let p = build_variant_playlist(12.0, &ABR_LADDER[0], "");
        assert!(p.contains("seg-0.ts"));
        assert!(p.contains("seg-1.ts"));
        assert!(!p.contains("seg-2.ts"));
    }

    #[test]
    fn variant_playlist_with_zero_duration_is_empty() {
        let p = build_variant_playlist(0.0, &ABR_LADDER[0], "");
        assert!(!p.contains("seg-"));
        assert!(p.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn variant_playlist_propagates_query_to_segments() {
        let p = build_variant_playlist(13.0, &ABR_LADDER[0], "?sub=abc");
        assert!(p.contains("seg-0.ts?sub=abc"));
        assert!(p.contains("seg-2.ts?sub=abc"));
    }

    #[test]
    fn master_playlist_lists_every_rendition() {
        let p = build_master_playlist("");
        for rendition in ABR_LADDER {
            assert!(p.contains(rendition.name));
            assert!(p.contains(&format!("{}x{}", rendition.width, rendition.height)));
        }
        assert!(p.contains("#EXT-X-STREAM-INF"));
        assert!(p.contains("CODECS="));
    }

    #[test]
    fn master_playlist_propagates_query_to_variant_urls() {
        let p = build_master_playlist("?sub=abc");
        assert!(p.contains("480p/playlist.m3u8?sub=abc"));
        assert!(p.contains("1080p/playlist.m3u8?sub=abc"));
    }
}
