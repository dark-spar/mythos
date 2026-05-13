//! JWT secret resolution.
//!
//! Order of preference:
//! 1. `MYTHOS_JWT_SECRET` env var (base64, decoded to ≥32 bytes).
//! 2. `{data_dir}/jwt.secret` (base64) if present.
//! 3. Generate 32 random bytes and persist to `{data_dir}/jwt.secret` atomically.
//!
//! On Unix the persisted file is chmod 0600 before the atomic rename. On
//! non-Unix we log a warning — the secret is still written but the OS
//! enforces no permissions.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand_core::{OsRng, RngCore};
use tracing::info;

const MIN_SECRET_LEN: usize = 32;

pub fn resolve(data_dir: &Path) -> Result<Vec<u8>> {
    if let Ok(env) = std::env::var("MYTHOS_JWT_SECRET") {
        let bytes = decode_base64(&env).context("decoding MYTHOS_JWT_SECRET")?;
        if bytes.len() < MIN_SECRET_LEN {
            bail!(
                "MYTHOS_JWT_SECRET decoded to {} bytes; need at least {}",
                bytes.len(),
                MIN_SECRET_LEN
            );
        }
        info!("using JWT secret from MYTHOS_JWT_SECRET env var");
        return Ok(bytes);
    }

    let path = secret_path(data_dir);
    if path.exists() {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("reading jwt secret at {}", path.display()))?;
        let bytes = decode_base64(contents.trim())
            .with_context(|| format!("decoding jwt secret at {}", path.display()))?;
        if bytes.len() < MIN_SECRET_LEN {
            bail!(
                "jwt secret at {} decoded to {} bytes; need at least {}",
                path.display(),
                bytes.len(),
                MIN_SECRET_LEN
            );
        }
        return Ok(bytes);
    }

    info!(path = %path.display(), "generating new JWT secret");
    let mut bytes = vec![0u8; MIN_SECRET_LEN];
    OsRng.fill_bytes(&mut bytes);
    write_atomic(&path, &BASE64.encode(&bytes))?;
    Ok(bytes)
}

pub fn secret_path(data_dir: &Path) -> PathBuf {
    data_dir.join("jwt.secret")
}

fn decode_base64(s: &str) -> Result<Vec<u8>> {
    BASE64.decode(s.trim()).context("base64 decode failed")
}

fn write_atomic(target: &Path, contents: &str) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("jwt secret target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("creating directory {}", parent.display()))?;

    let tmp = parent.join(format!(".jwt.secret.tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("creating temp file {}", tmp.display()))?;
        f.write_all(contents.as_bytes())
            .with_context(|| format!("writing temp file {}", tmp.display()))?;
        f.sync_all().ok();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    }
    #[cfg(not(unix))]
    {
        tracing::warn!(
            "jwt.secret written without strict file permissions on non-Unix platform; \
             ensure the data_dir is not world-readable"
        );
    }

    fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}
