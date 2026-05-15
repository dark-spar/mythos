//! Live HLS transcoding session manager.
//!
//! One ffmpeg subprocess per `(user_id, item_id, kind)` key, where
//! `kind` distinguishes movie sessions from episode sessions so a
//! movie UUID and an episode UUID with the same string never collide.
//! Sessions are identified by their `start_segment`: calling
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

use mythos_core::{PlaybackMode, ffmpeg_bin};
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info};
use uuid::Uuid;

use crate::abr::{Rendition, is_known_variant};
use crate::hwaccel::{HwAccel, TonemapConfig, TonemapSupport};

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

/// Which kind of media item a transcode session belongs to. Mirrors
/// the `/api/{movies,episodes}/{id}/hls/...` route split and keeps the
/// session work-dir layout self-describing.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ItemKind {
    Movie,
    Episode,
}

impl ItemKind {
    /// Lower-case slug for log lines, URL segments, and the on-disk
    /// session work-dir layout (`work_root/{user}/{kind}/{item_id}`).
    pub const fn as_slug(self) -> &'static str {
        match self {
            ItemKind::Movie => "movies",
            ItemKind::Episode => "episodes",
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SessionKey {
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub kind: ItemKind,
}

pub struct TranscodeSession {
    pub key: SessionKey,
    /// Global segment index this session's `seg-0.ts` represents.
    pub start_segment: u32,
    /// Absolute ffprobe stream index of the subtitle being burned in
    /// for this session, or `None` if subs are off. A request with a
    /// different value forces a restart, same as a backward seek.
    pub burn_in_sub: Option<i64>,
    /// Playback mode this session was launched for. Toggling
    /// direct-play/remux/transcode-* is a restart trigger because
    /// the ffmpeg invocation differs per mode.
    pub mode: PlaybackMode,
    /// Whether the filter graph for this session is doing HDR→SDR
    /// tonemapping, and which curve. Toggling either is a restart
    /// trigger because the filter graph differs.
    pub tonemap: TonemapConfig,
    /// Renditions emitted by ffmpeg in this session. Names match the
    /// `%v` subdirectories under `work_dir`. For copy modes this is
    /// a single source-resolution rendition; for ABR modes a subset
    /// of [`crate::ABR_LADDER`].
    pub renditions: Vec<Rendition>,
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
    /// Which HW tonemap filters the active ffmpeg actually has
    /// compiled in. Probed once at startup. The HLS handler reads
    /// this to silently downgrade an operator-selected
    /// [`crate::TonemapPipeline`] to [`crate::TonemapPipeline::Software`]
    /// when the named filter is missing, without rewriting the
    /// stored row (so a later ffmpeg upgrade restores the original
    /// intent).
    tonemap_support: TonemapSupport,
}

impl TranscodeManager {
    pub fn new(work_root: PathBuf, accel: HwAccel, tonemap_support: TonemapSupport) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                work_root,
                sessions: RwLock::new(HashMap::new()),
                accel,
                tonemap_support,
            }),
        }
    }

    pub fn hwaccel(&self) -> HwAccel {
        self.inner.accel
    }

    /// Which HW tonemap filters this server actually has available.
    /// The HLS handler combines this with the operator's chosen
    /// pipeline to decide whether to honour the request or downgrade
    /// to [`crate::TonemapPipeline::Software`].
    pub fn tonemap_support(&self) -> TonemapSupport {
        self.inner.tonemap_support
    }

    /// Return a session that's either currently transcoding segment
    /// `seg_idx` or will be very shortly. Restarts the session if any
    /// of the following differ from the existing one: `seg_idx`
    /// (before-start or too-far-ahead), `burn_in_sub`, `mode`,
    /// `tonemap`, or the rendition list.
    ///
    /// If the existing session is younger than
    /// [`RESTART_GRACE_PERIOD`] and the requested segment isn't
    /// compatible with it, returns [`TranscodeError::SessionStillBooting`]
    /// rather than killing the in-flight ffmpeg. The HTTP layer turns
    /// that into a 503 + `Retry-After`, which protects the manager
    /// from clients (forgotten tabs, video extensions, etc.) issuing
    /// rapid segment requests across the timeline.
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_session_for_segment(
        &self,
        key: SessionKey,
        input_path: &Path,
        variant: &str,
        seg_idx: u32,
        burn_in_sub: Option<i64>,
        mode: PlaybackMode,
        tonemap: TonemapConfig,
        renditions: &[Rendition],
    ) -> Result<Arc<TranscodeSession>, TranscodeError> {
        if mode == PlaybackMode::DirectPlay {
            // The HLS pipeline only services transcoding/remuxing
            // modes; direct-play is served by a separate handler.
            return Err(TranscodeError::InvalidVariant(
                "direct_play has no transcode session".to_string(),
            ));
        }
        if !renditions.iter().any(|r| r.name == variant) {
            return Err(TranscodeError::InvalidVariant(variant.to_string()));
        }
        // Fast path: existing session covers this segment AND has the
        // same subtitle / mode / tonemap / rendition selection.
        let needs_restart = {
            let sessions = self.inner.sessions.read().await;
            match sessions.get(&key) {
                Some(existing)
                    if existing.burn_in_sub != burn_in_sub
                        || existing.mode != mode
                        || existing.tonemap != tonemap
                        || !rendition_names_match(&existing.renditions, renditions) =>
                {
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
        self.restart_at(
            key,
            input_path,
            seg_idx,
            burn_in_sub,
            mode,
            tonemap,
            renditions,
        )
        .await
    }

    /// Start a fresh session at `seg_idx`, killing any existing session
    /// under the same key.
    #[allow(clippy::too_many_arguments)]
    async fn restart_at(
        &self,
        key: SessionKey,
        input_path: &Path,
        seg_idx: u32,
        burn_in_sub: Option<i64>,
        mode: PlaybackMode,
        tonemap: TonemapConfig,
        renditions: &[Rendition],
    ) -> Result<Arc<TranscodeSession>, TranscodeError> {
        let mut sessions = self.inner.sessions.write().await;
        if let Some(existing) = sessions.remove(&key) {
            debug!(
                user = %existing.key.user_id,
                kind = existing.key.kind.as_slug(),
                item = %existing.key.item_id,
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
            .join(key.kind.as_slug())
            .join(key.item_id.to_string());
        tokio::fs::create_dir_all(&work_dir).await?;
        // ffmpeg's HLS muxer doesn't auto-create per-variant subdirs
        // when `%v` is in `-hls_segment_filename`; pre-create them.
        for rendition in renditions {
            tokio::fs::create_dir_all(work_dir.join(rendition.name)).await?;
        }

        let offset_seconds = f64::from(seg_idx) * SEGMENT_DURATION_SECS;
        // Force the CPU pipeline when burning in image subs (the
        // overlay-on-VAAPI path is brittle), and for any mode that
        // doesn't re-encode video (the HW path is wasted overhead
        // and `-c:v copy` doesn't go through the encoder anyway).
        let session_accel = match (burn_in_sub, mode) {
            (Some(_), _) => HwAccel::Cpu,
            (_, PlaybackMode::Remux | PlaybackMode::TranscodeAudio) => HwAccel::Cpu,
            _ => self.inner.accel,
        };
        info!(
            user = %key.user_id,
            kind = key.kind.as_slug(),
            item = %key.item_id,
            seg_idx,
            offset_seconds,
            mode = mode.as_str(),
            encoder = session_accel.h264_encoder(),
            renditions = renditions.len(),
            burn_in_sub = ?burn_in_sub,
            tonemap = tonemap.apply,
            tonemap_algo = tonemap.algorithm.as_str(),
            "starting ffmpeg transcode session"
        );

        let child = launch_ffmpeg(
            input_path,
            &work_dir,
            offset_seconds,
            session_accel,
            burn_in_sub,
            mode,
            tonemap,
            renditions,
        )
        .await
        .map_err(TranscodeError::Spawn)?;

        let session = Arc::new(TranscodeSession {
            key: key.clone(),
            start_segment: seg_idx,
            burn_in_sub,
            mode,
            tonemap,
            renditions: renditions.to_vec(),
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
                kind = session.key.kind.as_slug(),
                item = %session.key.item_id,
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

#[allow(clippy::too_many_arguments)]
async fn launch_ffmpeg(
    input: &Path,
    work_dir: &Path,
    offset_seconds: f64,
    accel: HwAccel,
    burn_in_sub: Option<i64>,
    mode: PlaybackMode,
    tonemap: TonemapConfig,
    renditions: &[Rendition],
) -> std::io::Result<Child> {
    // Per-variant subdirs are pre-created by the caller. ffmpeg
    // expands `%v` in `-hls_segment_filename` and the output URL to
    // the variant name set in `-var_stream_map`.
    let segment_pattern = work_dir.join("%v").join("seg-%d.ts");
    let variant_playlist_pattern = work_dir.join("%v").join("playlist.m3u8");
    let copy_video = matches!(mode, PlaybackMode::Remux | PlaybackMode::TranscodeAudio);
    let copy_audio = matches!(mode, PlaybackMode::Remux | PlaybackMode::TranscodeVideo);

    let mut cmd = Command::new(ffmpeg_bin());
    cmd.arg("-hide_banner").arg("-loglevel").arg("warning");
    if !copy_video {
        // HW decode flags only matter when we're going to re-encode.
        // For `-c:v copy`, the bitstream passes through and we want
        // to avoid forcing the GPU pipeline.
        cmd.args(accel.decode_args(tonemap.pipeline));
    }
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

    // Filter graph only applies when we're re-encoding video.
    if !copy_video {
        cmd.arg("-filter_complex").arg(build_filter_complex(
            accel,
            burn_in_sub,
            tonemap,
            renditions,
        ));
    }

    // Map: per output index, the right video + audio source.
    for i in 0..renditions.len() {
        if copy_video {
            cmd.arg("-map").arg("0:v:0");
        } else {
            cmd.arg("-map").arg(format!("[v{i}]"));
        }
        cmd.arg("-map").arg("0:a:0?");
    }

    // Video encoder args, per output, or `-c:v copy` when keeping
    // the source bitstream.
    if copy_video {
        for i in 0..renditions.len() {
            cmd.arg(format!("-c:v:{i}")).arg("copy");
        }
    } else {
        for (i, rendition) in renditions.iter().enumerate() {
            cmd.args(accel.abr_video_encoder_args(i, rendition));
        }
    }

    // Audio: copy bitstream or re-encode to AAC stereo.
    if copy_audio {
        for i in 0..renditions.len() {
            cmd.arg(format!("-c:a:{i}")).arg("copy");
        }
    } else {
        cmd.arg("-c:a").arg("aac").arg("-ac").arg("2");
        for (i, rendition) in renditions.iter().enumerate() {
            cmd.arg(format!("-b:a:{i}"))
                .arg(format!("{}k", rendition.audio_bitrate_kbps));
        }
    }

    // PTS preservation. `-force_key_frames` is only meaningful when
    // we're actually encoding video — with `-c:v copy` the keyframes
    // are wherever the source put them, and ffmpeg uses the nearest
    // one as each segment boundary (segments end up uneven, which
    // hls.js tolerates).
    cmd.arg("-copyts")
        .arg("-muxdelay")
        .arg("0")
        .arg("-muxpreload")
        .arg("0");
    if !copy_video {
        cmd.arg("-force_key_frames")
            .arg(format!("expr:gte(t,n_forced*{SEGMENT_DURATION_SECS})"));
    }

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
        .arg(build_var_stream_map(renditions))
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

/// Construct the `-filter_complex` argument that optionally tonemaps
/// HDR→SDR, optionally burns a subtitle stream into the video, then
/// splits the result N ways and scales each branch to one rendition.
/// Only used when video is being re-encoded.
///
/// Filter ordering, when both tonemap and burn-in apply:
///
/// ```text
/// [0:v]<tonemap>[tm];[tm][0:N]overlay[base];[base]split=N[s0]...
/// ```
///
/// Tonemap runs **before** the subtitle overlay so the burnt-in
/// pixels composite onto already-SDR video. Compositing first and
/// then tonemapping would dim the subtitle's white-on-black bitmap
/// into a washed-out gray.
fn build_filter_complex(
    accel: HwAccel,
    burn_in_sub: Option<i64>,
    tonemap: TonemapConfig,
    renditions: &[Rendition],
) -> String {
    let n = renditions.len();
    let mut graph = String::new();

    // Stage 1: optional tonemap. Output label `[tm]` only when we
    // actually emit a chain; otherwise downstream stages read from
    // `[0:v]` directly.
    let post_tonemap = if tonemap.apply {
        let chain = accel.tonemap_prefilter(tonemap);
        graph.push_str(&format!("[0:v]{chain}[tm];"));
        "[tm]"
    } else {
        "[0:v]"
    };

    // Stage 2: optional subtitle overlay. PGS/VOBSUB bitmap streams
    // auto-convert when used as overlay's second input; absolute
    // stream indexing keeps multi-track files unambiguous.
    let split_input: String = match burn_in_sub {
        Some(stream_index) => {
            graph.push_str(&format!("{post_tonemap}[0:{stream_index}]overlay[base];"));
            "[base]".to_string()
        }
        None => post_tonemap.to_string(),
    };

    // Stage 3: per-rendition scale, fanned out by `split` when n > 1.
    if n == 1 {
        graph.push_str(&format!(
            "{split_input}{}[v0]",
            accel.scale_filter(&renditions[0])
        ));
        return graph;
    }

    let split_labels: String = (0..n).map(|i| format!("[s{i}]")).collect();
    graph.push_str(&format!("{split_input}split={n}{split_labels}"));

    for (i, rendition) in renditions.iter().enumerate() {
        graph.push(';');
        graph.push_str(&format!("[s{i}]{}[v{i}]", accel.scale_filter(rendition)));
    }
    graph
}

/// `-var_stream_map` value: pairs output indices to variant names.
fn build_var_stream_map(renditions: &[Rendition]) -> String {
    renditions
        .iter()
        .enumerate()
        .map(|(i, r)| format!("v:{i},a:{i},name:{}", r.name))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return `true` if the two rendition slices describe the same set of
/// variants (by name, order-sensitive). Bitrate / dimension drift on
/// the same name still counts as a match — the same session can
/// keep running.
fn rendition_names_match(a: &[Rendition], b: &[Rendition]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.name == y.name)
}

/// Parse `seg-N.ts` into `N`. Anything else (including
/// `playlist.m3u8`) returns `None`.
pub fn parse_segment_filename(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg-")?.strip_suffix(".ts")?;
    rest.parse::<u32>().ok()
}

/// Synthetic ABR master playlist. Lists each provided rendition
/// with its declared bandwidth + resolution + codecs hint. Relative
/// URLs point at `:variant/playlist.m3u8` served by the API; the
/// player fetches whichever it picks.
///
/// `query` is appended verbatim to each variant URL (e.g.
/// `"?mode=full&sub=<uuid>"`), so selections from the master URL
/// propagate to the variant playlist requests. Relative URL
/// resolution in HLS clients drops the query otherwise.
pub fn build_master_playlist(renditions: &[Rendition], query: &str) -> String {
    let mut p = String::new();
    p.push_str("#EXTM3U\n");
    p.push_str("#EXT-X-VERSION:6\n");
    p.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    for r in renditions {
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
    use crate::hwaccel::TonemapAlgorithm;

    #[test]
    fn session_key_discriminates_movie_from_episode_with_same_uuid() {
        // The whole point of ItemKind in SessionKey: a movie and an
        // episode with happen-to-be-identical UUIDs (astronomically
        // rare in practice but still possible) get distinct sessions
        // in the manager's HashMap.
        use std::collections::HashSet;
        let user = Uuid::now_v7();
        let id = Uuid::now_v7();
        let movie = SessionKey {
            user_id: user,
            item_id: id,
            kind: ItemKind::Movie,
        };
        let episode = SessionKey {
            user_id: user,
            item_id: id,
            kind: ItemKind::Episode,
        };
        assert_ne!(movie, episode);
        let set: HashSet<_> = [movie, episode].into_iter().collect();
        assert_eq!(set.len(), 2);
    }

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
        let p = build_variant_playlist(13.0, &crate::ABR_LADDER[0], ""); // 6 + 6 + 1
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
        let p = build_variant_playlist(12.0, &crate::ABR_LADDER[0], "");
        assert!(p.contains("seg-0.ts"));
        assert!(p.contains("seg-1.ts"));
        assert!(!p.contains("seg-2.ts"));
    }

    #[test]
    fn variant_playlist_with_zero_duration_is_empty() {
        let p = build_variant_playlist(0.0, &crate::ABR_LADDER[0], "");
        assert!(!p.contains("seg-"));
        assert!(p.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn variant_playlist_propagates_query_to_segments() {
        let p = build_variant_playlist(13.0, &crate::ABR_LADDER[0], "?sub=abc");
        assert!(p.contains("seg-0.ts?sub=abc"));
        assert!(p.contains("seg-2.ts?sub=abc"));
    }

    #[test]
    fn master_playlist_lists_every_rendition() {
        let p = build_master_playlist(crate::ABR_LADDER, "");
        for rendition in crate::ABR_LADDER {
            assert!(p.contains(rendition.name));
            assert!(p.contains(&format!("{}x{}", rendition.width, rendition.height)));
        }
        assert!(p.contains("#EXT-X-STREAM-INF"));
        assert!(p.contains("CODECS="));
    }

    #[test]
    fn master_playlist_propagates_query_to_variant_urls() {
        let p = build_master_playlist(crate::ABR_LADDER, "?sub=abc");
        assert!(p.contains("480p/playlist.m3u8?sub=abc"));
        assert!(p.contains("1080p/playlist.m3u8?sub=abc"));
    }

    #[test]
    fn master_playlist_subset_drops_unincluded_tiers() {
        let only_720 = vec![crate::ABR_LADDER[1]];
        let p = build_master_playlist(&only_720, "");
        assert!(p.contains("720p"));
        assert!(!p.contains("480p"));
        assert!(!p.contains("1080p"));
    }

    #[test]
    fn filter_complex_without_tonemap_omits_zscale() {
        let graph = build_filter_complex(
            HwAccel::Cpu,
            None,
            TonemapConfig::default(),
            crate::ABR_LADDER,
        );
        assert!(!graph.contains("zscale"));
        assert!(!graph.contains("tonemap="));
        assert!(graph.starts_with("[0:v]split="));
    }

    #[test]
    fn filter_complex_with_tonemap_places_tonemap_before_split() {
        let cfg = TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Hable,
            ..TonemapConfig::default()
        };
        let graph = build_filter_complex(HwAccel::Cpu, None, cfg, crate::ABR_LADDER);
        // [0:v]<tonemap>[tm];[tm]split=...
        assert!(graph.starts_with("[0:v]zscale"));
        assert!(graph.contains("tonemap=tonemap=hable"));
        let tm_idx = graph.find("[tm];").expect("tonemap output label");
        let split_idx = graph.find("split=").expect("split present");
        assert!(
            tm_idx < split_idx,
            "tonemap must come before the rendition fan-out"
        );
    }

    #[test]
    fn filter_complex_tonemap_runs_before_subtitle_overlay() {
        // Tonemap-then-overlay is the right order: compositing a
        // white-on-black bitmap onto an HDR frame and then tonemapping
        // would dim the subtitle into washed-out gray.
        let cfg = TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Hable,
            ..TonemapConfig::default()
        };
        let graph = build_filter_complex(HwAccel::Cpu, Some(3), cfg, crate::ABR_LADDER);
        let tonemap_idx = graph.find("tonemap=").expect("tonemap present");
        let overlay_idx = graph.find("overlay").expect("overlay present");
        assert!(
            tonemap_idx < overlay_idx,
            "tonemap must run before subtitle overlay"
        );
    }
}
