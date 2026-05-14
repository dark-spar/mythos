# CLAUDE.md

Notes for AI assistants working in this repo. Keep this file short and current.

## What this project is

Mythos is an open-source self-hosted media server written in Rust, comparable to
Jellyfin / Plex. Target media: movies, TV, music, photos, books. Distribution
goal is a single self-contained binary with the web UI baked in.

## Two strategic decisions to keep in mind

1. **Hybrid frontend.** Build a clean, opinionated REST API plus a built-in
   SvelteKit web UI first. A Jellyfin-API-compatibility shim comes in a later
   phase so existing clients (Findroid, Swiftfin, jellyfin-web) work against
   Mythos without us being shackled to Jellyfin's data model on day one.
2. **Streaming is phased.** Direct play (HTTP byte-range) → FFmpeg HLS
   transcoding → device profiles + ABR + hardware acceleration. Don't try to
   build the profile resolver early — it's the hardest piece of any media
   server and isn't needed for an MVP.

## Layout

```
crates/
  mythos-server   binary; axum app, embeds the SPA, wires everything together
  mythos-core     domain types shared across crates (MediaItem, MediaKind, ...)
  mythos-db       SQLite pool + migrate runner. Re-exports SqlitePool.
  mythos-auth     argon2 password hashing, JWT issuance, AuthUser/AdminUser extractors
  mythos-api      axum routers/handlers
  mythos-scan     filesystem walker (jwalk) + ffprobe identifier
  mythos-meta     TMDb client w/ on-disk poster cache (MusicBrainz/OpenLibrary later)
  mythos-stream   FFmpeg HLS transcoder, ABR ladder, hwaccel probe, subtitle burn-in
migrations/       SQL files run by sqlx::migrate! (0001–0007 so far)
web/              SvelteKit 2 + Svelte 5 + TS + Tailwind v4 + Vidstack + hls.js
```

Workspace dependencies live in the root `Cargo.toml` under `[workspace.dependencies]`;
member crates reference them with `dep.workspace = true` rather than re-pinning
versions. Add new shared deps there.

## How the SPA gets into the binary

`crates/mythos-server/build.rs` runs `pnpm install` + `pnpm build` in `web/`,
producing `web/build/`. `rust_embed` bakes that directory into the binary at
compile time. The fallback handler in `crates/mythos-server/src/web.rs` serves
embedded assets for known paths and falls back to `index.html` for unknown
paths so client-side routing works.

The `MYTHOS_SKIP_WEB_BUILD=1` env var short-circuits `pnpm build`. Use it when:
- iterating on Rust only and `web/build/` is already current
- running `cargo check` / `cargo clippy` / `cargo test` (CI sets it)
- the SPA has been built by a separate CI step that hands off `web/build/`

In debug builds, `rust_embed` reads from disk at runtime, so SPA changes show
up after `pnpm build` without a Rust rebuild.

## Build & run

```sh
cargo run --bin mythos-server                    # full build incl. SPA, http://127.0.0.1:8080
MYTHOS_SKIP_WEB_BUILD=1 cargo run --bin mythos-server   # Rust-only iteration
cd web && pnpm dev                                # UI dev server (point its proxy at :8080 when wiring real API calls)
```

Lint/test loop before committing:

```sh
MYTHOS_SKIP_WEB_BUILD=1 cargo fmt --all -- --check
MYTHOS_SKIP_WEB_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings
MYTHOS_SKIP_WEB_BUILD=1 cargo test --workspace
( cd web && pnpm lint && pnpm check && pnpm test )
```

## Conventions

- **Edition 2024**, MSRV 1.95 (pinned in `rust-toolchain.toml`).
- **License**: MIT. New files should be added under that license; no
  per-file headers required.
- **Commits**: conventional-style subject (`feat:`, `fix:`, `refactor:`, …),
  imperative mood, body explaining *why*, not *what*.
- **Errors**: `thiserror` for library crates (typed errors crossing crate
  boundaries), `anyhow` only at the binary boundary in `mythos-server`.
- **IDs**: UUID v7 for all primary keys, stored as `TEXT` in SQLite.
- **Time**: ISO-8601 UTC strings in the DB (`strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`),
  `chrono::DateTime<Utc>` in Rust.

## Phased roadmap

Track which phase we're in; don't pull work forward without a reason.

- **Phase 0 — foundation (done).** Workspace, embedded SPA, axum boot,
  SQLite + migrations, `/api/health`, CI.
- **Phase 1 — library scan + browse + auth (done, movies-only slice).**
  argon2 + JWT auth, libraries CRUD, `mythos-scan` walker (jwalk + ffprobe),
  `mythos-meta` TMDb client with on-disk poster cache + admin-editable API key
  (env var wins; UI value swaps live via `TmdbHandle`). UI: login, libraries
  admin, movie grid, movie detail. Schema for episodes/series/tracks/albums/
  artists/photos/books is **not yet built** — comes in Phase 3.
- **Phase 2 — direct-play streaming (done).** `GET /api/movies/:id/stream`
  byte-range, Vidstack binding, debounced watch progress, resume.
- **Phase 3 — TV, music, photos, books (NEXT).** Series hierarchy, music tags
  via `lofty` + MusicBrainz, photo thumbnails + EXIF, EPUB metadata + `epub.js`.
- **Phase 4 — HLS transcoding (done).** FFmpeg subprocess manager,
  segmented HLS, seek-by-restart.
- **Phase 5 — profiles + ABR + hwaccel (done through 5d).** Hardware probe
  (NVENC / QSV / VAAPI / VideoToolbox), multi-rendition ABR ladder, subtitle
  burn-in + WebVTT sidecar, client-declared profile + transcode taxonomy.
- **Phase 6 — Jellyfin compatibility shim.** Translation layer at `/jellyfin/*`
  for Findroid / Swiftfin / jellyfin-web.

The full plan with rationale and verification steps lives at
`~/.claude/plans/i-want-to-build-abundant-lamport.md` (not committed).

## Sharp edges

- **Database paths.** `mythos-db::open` calls `create_dir_all` on the parent of
  the DB path; respect the `data_dir` config. Don't hard-code paths in tests —
  use `tempfile::TempDir` and `:memory:` URLs where possible.
- **`figment::Error` is large.** Wrap in `Box` at API boundaries to satisfy
  `clippy::result_large_err` (already done in `Config::load`).
- **`rust_embed` folder path** is relative to the crate's `Cargo.toml`, not the
  workspace root — `crates/mythos-server` uses `../../web/build`. Don't break that.
- **CI runs the web build separately** from the Rust build (artifact handoff)
  so set `MYTHOS_SKIP_WEB_BUILD=1` for any cargo invocation in CI.
- **TMDb API key has two sources.** `MYTHOS_TMDB_API_KEY` env var wins over the
  `settings` table value (set via the admin UI). Resolution lives in
  `mythos_api::resolve_tmdb_api_key`; the live `TmdbHandle` is swapped by the
  settings PUT handler so a new key takes effect on the next scan without a
  restart. Don't reintroduce a config-only path.
- **`mythos-auth` errors don't implement `IntoResponse`.** Translation to HTTP
  lives in `mythos-api` so the auth crate stays usable from non-HTTP contexts
  (CLI, tests, future Jellyfin shim). Keep that boundary.
- **HLS sessions are torn down on player teardown** (`DELETE /api/movies/:id/hls`).
  Don't leak ffmpeg subprocesses — route any new transcode entry point through
  `TranscodeManager` so the lifecycle stays centralized.
