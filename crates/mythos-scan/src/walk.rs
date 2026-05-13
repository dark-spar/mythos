//! Directory walker that yields video file paths under a root.
//!
//! Backed by `jwalk` for parallel directory iteration. Files are
//! filtered to a small whitelist of container extensions; the list is
//! conservative to avoid pulling in `.nfo`, `.srt`, or thumbnail
//! sidecars during a movie scan.

use std::path::{Path, PathBuf};

use jwalk::WalkDir;

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "webm", "ts", "m2ts", "wmv",
];

pub fn video_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .skip_hidden(true)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path())
        .filter(|path| is_video(path))
        .collect()
}

pub fn is_video(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    VIDEO_EXTENSIONS.iter().any(|e| *e == lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn picks_up_known_video_extensions_and_skips_sidecars() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("a.mkv"), b"").unwrap();
        fs::write(root.join("b.MP4"), b"").unwrap(); // uppercase ext
        fs::write(root.join("c.nfo"), b"").unwrap();
        fs::write(root.join("d.srt"), b"").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("e.mov"), b"").unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden").join("f.mkv"), b"").unwrap();

        let mut paths: Vec<String> = video_files(root)
            .into_iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        paths.sort();

        let normalized: Vec<String> = paths.into_iter().map(|p| p.replace('\\', "/")).collect();
        assert_eq!(normalized, vec!["a.mkv", "b.MP4", "sub/e.mov"]);
    }
}
