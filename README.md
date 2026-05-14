# Mythos

A self-hosted media server, written in Rust. Movies, TV, music, photos, books — one binary, your library, your hardware.

> Status: **early development.** Phase 0 (foundation) is in place: workspace, embedded SvelteKit UI, SQLite + migrations, axum server. Library scanning, metadata, and streaming arrive in subsequent phases.

## Architecture at a glance

- **Server**: Rust workspace, async via Tokio, HTTP via [axum](https://github.com/tokio-rs/axum), persistence via [SQLx](https://github.com/launchbadge/sqlx) + SQLite.
- **Web UI**: SvelteKit 2 + Svelte 5 + Tailwind v4, embedded into the server binary via [`rust-embed`](https://github.com/pyrossh/rust-embed) at build time. Media playback uses [Vidstack](https://www.vidstack.io/) and [hls.js](https://github.com/video-dev/hls.js).
- **Streaming** (planned): direct play first, FFmpeg HLS transcoding next, then device profiles, ABR, and hardware acceleration.
- **Compatibility shim** (later phase): a Jellyfin-API-compatible router so existing clients (Findroid, Swiftfin, jellyfin-web) work against Mythos.

## Workspace layout

```
crates/
  mythos-server   — main binary, axum app, embedded SPA fallback
  mythos-core     — shared domain types
  mythos-db       — SQLite pool + migrations
  mythos-scan     — filesystem scanner (Phase 1)
  mythos-meta     — TMDb / MusicBrainz / OpenLibrary clients (Phase 1)
  mythos-stream   — direct-play + HLS pipeline (Phase 2/4)
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

Phase 0 — foundation (done)
Phase 1 — library scan + browse + auth
Phase 2 — direct-play streaming
Phase 3 — TV, music, photos, books
Phase 4 — HLS transcoding
Phase 5 — device profiles + ABR + hardware acceleration
Phase 6 — Jellyfin-API compatibility shim

## License

MIT — see [LICENSE](LICENSE).
