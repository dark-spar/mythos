# Mythos

A self-hosted media server, written in Rust. Movies, TV, music, photos, books — one binary, your library, your hardware.

> Status: **early development, movies-only.** Auth, library CRUD, filesystem scanning, TMDb enrichment, direct-play streaming, watch progress/resume, FFmpeg HLS transcoding, multi-rendition ABR, hardware acceleration, and subtitle burn-in are all working. TV/music/photos/books and a Jellyfin compatibility shim are next.

## Architecture at a glance

- **Server**: Rust workspace, async via Tokio, HTTP via [axum](https://github.com/tokio-rs/axum), persistence via [SQLx](https://github.com/launchbadge/sqlx) + SQLite.
- **Web UI**: SvelteKit 2 + Svelte 5 + Tailwind v4, embedded into the server binary via [`rust-embed`](https://github.com/pyrossh/rust-embed) at build time. Media playback uses [Vidstack](https://www.vidstack.io/) and [hls.js](https://github.com/video-dev/hls.js).
- **Streaming**: HTTP byte-range direct play for browser-compatible files, FFmpeg HLS transcoding for everything else, multi-rendition ABR ladder, hardware acceleration (NVENC / QSV / VAAPI / VideoToolbox), subtitle burn-in for image subs.
- **Auth**: argon2id password hashing, HS256 JWTs delivered via `SameSite=Lax` HttpOnly cookie (or `Authorization: Bearer` for API clients).
- **Compatibility shim** (later phase): a Jellyfin-API-compatible router so existing clients (Findroid, Swiftfin, jellyfin-web) work against Mythos.

## Workspace layout

```
crates/
  mythos-server   — main binary, axum app, embedded SPA fallback
  mythos-core     — shared domain types
  mythos-db       — SQLite pool + migrations
  mythos-auth     — argon2 + JWT, AuthUser/AdminUser extractors
  mythos-scan     — filesystem scanner (jwalk + ffprobe)
  mythos-meta     — TMDb client (MusicBrainz / OpenLibrary later)
  mythos-stream   — FFmpeg HLS transcoder, ABR, hwaccel, subtitle burn-in
  mythos-api      — HTTP handlers
migrations/       — SQL files run by sqlx::migrate!
web/              — SvelteKit project; pnpm build → web/build/ → embedded
```

## Quickstart (development)

Prerequisites: **Rust 1.95+**, **Node 22+**, **pnpm 10+**, **ffmpeg / ffprobe** (for later phases).

```sh
# Build everything (Rust + SPA in one shot — build.rs invokes pnpm build).
cargo run --bin mythos-server
```

Then open http://127.0.0.1:8080. The embedded SPA loads and pings `/api/health`.

### Iterating on Rust only

If you don't want `cargo build` to re-run `pnpm build` every time:

```sh
MYTHOS_SKIP_WEB_BUILD=1 cargo run --bin mythos-server
```

### Iterating on the UI only

```sh
cd web
pnpm dev
```

The Vite dev server proxies to nothing by default; configure it to point at the Rust server (port 8080) when you wire up real API calls.

## Configuration

Mythos loads settings from (highest to lowest priority):

1. `MYTHOS_*` environment variables (e.g. `MYTHOS_LISTEN=0.0.0.0:8080`)
2. A TOML file at `MYTHOS_CONFIG`, or `./mythos.toml` if present
3. Built-in defaults

Example `mythos.toml`:

```toml
listen     = "0.0.0.0:8080"
data_dir   = "/var/lib/mythos"
log_filter = "info,mythos=debug,sqlx=warn"
```

## Roadmap

- Phase 0 — foundation ✅
- Phase 1 — library scan + browse + auth ✅ *(movies-only slice)*
- Phase 2 — direct-play streaming ✅
- Phase 3 — TV, music, photos, books ⏳ *next*
- Phase 4 — HLS transcoding ✅
- Phase 5 — device profiles + ABR + hardware acceleration ✅ *(through 5d)*
- Phase 6 — Jellyfin-API compatibility shim

## License

MIT — see [LICENSE](LICENSE).
