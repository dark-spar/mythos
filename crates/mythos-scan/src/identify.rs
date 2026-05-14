//! Filename-based movie identifier.
//!
//! Strategy:
//! 1. Strip the file extension; try to parse "<title>...<year>" out of
//!    the stem.
//! 2. If no year was found, try the parent directory name (common layout:
//!    `Movies/The Matrix (1999)/the.matrix.mkv`).
//! 3. Fall back to a cleaned-up filename with no year.
//!
//! This is a Phase 1c "good enough" identifier. Edge cases like
//! release-group suffixes, multi-year filenames, and odd separators are
//! deliberately not handled — operators can correct metadata in
//! Phase 1d via TMDb override.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub title: String,
    pub year: Option<i64>,
}

pub fn identify_movie(path: &Path) -> Identity {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    parse(stem)
        .or_else(|| parse(parent))
        .unwrap_or_else(|| Identity {
            title: clean(stem),
            year: None,
        })
}

fn parse(s: &str) -> Option<Identity> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Title is non-greedy up to a separator-then-year boundary.
        // Requiring a separator after the year prevents "2001" in
        // "2001 A Space Odyssey 1968" from being grabbed as the year.
        Regex::new(r"(?i)^(?P<title>.+?)[\s._\-]+\(?(?P<year>19\d{2}|20\d{2})\)?(?:[\s._\-]|$)")
            .expect("static identifier regex")
    });
    let caps = re.captures(s)?;
    let title = clean(caps.name("title")?.as_str());
    if title.is_empty() {
        return None;
    }
    let year = caps
        .name("year")
        .and_then(|m| m.as_str().parse::<i64>().ok());
    Some(Identity { title, year })
}

/// Collapse `.`, `_`, `-` to spaces and squeeze whitespace runs.
///
/// Shared with `identify_tv.rs` — cleaning is the same regardless of
/// what came out of the upstream regex match.
pub(crate) fn clean(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '.' | '_' | '-' => ' ',
            _ => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn id(path: &str) -> Identity {
        identify_movie(&PathBuf::from(path))
    }

    #[test]
    fn parens_year() {
        let i = id("/movies/The Matrix (1999).mkv");
        assert_eq!(i.title, "The Matrix");
        assert_eq!(i.year, Some(1999));
    }

    #[test]
    fn dotted_release_name() {
        let i = id("/movies/The.Matrix.1999.1080p.BluRay.x264-GROUP.mkv");
        assert_eq!(i.title, "The Matrix");
        assert_eq!(i.year, Some(1999));
    }

    #[test]
    fn year_in_title_uses_real_year() {
        let i = id("/movies/2001 A Space Odyssey 1968.mkv");
        assert_eq!(i.title, "2001 A Space Odyssey");
        assert_eq!(i.year, Some(1968));
    }

    #[test]
    fn falls_back_to_parent_directory() {
        let i = id("/movies/The Matrix (1999)/the.matrix.mkv");
        assert_eq!(i.title, "The Matrix");
        assert_eq!(i.year, Some(1999));
    }

    #[test]
    fn no_year_yields_cleaned_title() {
        let i = id("/photos/family_reunion.mkv");
        assert_eq!(i.title, "family reunion");
        assert_eq!(i.year, None);
    }

}
