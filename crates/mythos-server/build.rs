//! Builds the SvelteKit SPA so `cargo build` produces a self-contained binary.
//!
//! Set `MYTHOS_SKIP_WEB_BUILD=1` to skip — useful when iterating on Rust only,
//! or when CI builds the web bundle in a separate step.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=MYTHOS_SKIP_WEB_BUILD");

    if env::var("MYTHOS_SKIP_WEB_BUILD").is_ok() {
        println!("cargo:warning=mythos-server: MYTHOS_SKIP_WEB_BUILD set, skipping pnpm build");
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let web_root = manifest_dir
        .join("../../web")
        .canonicalize()
        .expect("could not resolve web/ from mythos-server");

    // Watch the source files; node_modules and build outputs are ignored.
    for sub in [
        "src",
        "static",
        "package.json",
        "pnpm-lock.yaml",
        "svelte.config.js",
        "vite.config.ts",
        "tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={}", web_root.join(sub).display());
    }

    let pnpm = pnpm_command();

    run(
        Command::new(&pnpm)
            .args(["install", "--prefer-offline"])
            .current_dir(&web_root),
        "pnpm install",
    );
    run(
        Command::new(&pnpm).args(["build"]).current_dir(&web_root),
        "pnpm build",
    );
}

fn pnpm_command() -> String {
    // On Windows, pnpm ships as pnpm.cmd; everywhere else it's pnpm.
    if cfg!(windows) { "pnpm.cmd" } else { "pnpm" }.to_string()
}

fn run(cmd: &mut Command, label: &str) {
    let status = cmd.status().unwrap_or_else(|e| {
        panic!("{label} failed to start: {e}. Install pnpm or set MYTHOS_SKIP_WEB_BUILD=1.")
    });
    assert!(status.success(), "{label} exited with {status}");
}
