//! Filename-based TV identifier.
//!
//! Pattern priority (first match wins):
//! 1. **SxxEyy in the filename.** Tolerates `S01E01`, `S02 E01`,
//!    `S01.E01`, `S01EP01`, and the `S01E01E02` multi-episode form
//!    (the first episode wins; multi-ep playback handling is deferred).
//! 2. **NxMM in the filename.** Lowercase `x`; the episode must be at
//!    least two digits (`1x3` is ambiguous with timestamps and
//!    rejected).
//! 3. **Parent dir gives the season number** (`S01`, `Season 01`,
//!    `Specials`) **and the filename has a leading `E\d`**
//!    (`E01 - Title.mkv`).
//!
//! Series title comes from the nearest parent directory that doesn't
//! look like a season dir, a "Show S02"-style season-bearing folder,
//! or a junk sidecar dir (`Sample`, `Extras`, `Bonus`, `Featurettes`,
//! `Cover`, `Menu`, `Trailers`, `Intros`). Years are extracted from
//! `(YYYY)`-suffixed directory names via [`extract_trailing_year`].
//!
//! Returns `None` when nothing usable can be parsed. The scanner logs
//! the file at WARN and continues; it does not abort the run.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::identify::clean;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvIdentity {
    pub series: String,
    pub year: Option<i64>,
    pub season_number: i64,
    pub episode_number: i64,
    pub episode_title: Option<String>,
}

pub fn identify_tv(path: &Path) -> Option<TvIdentity> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;

    if let Some(m) = parse_season_episode(stem) {
        let (series, year) = series_and_year(path, m.prefix);
        if series.is_empty() {
            return None;
        }
        return Some(TvIdentity {
            series,
            year,
            season_number: m.season_number,
            episode_number: m.episode_number,
            episode_title: extract_episode_title(m.suffix),
        });
    }

    if let Some((season_number, episode_number, suffix)) = parse_from_parent_season(path, stem) {
        // The immediate parent is the season dir; walk past it.
        let (series, year) = match walk_series_dir(path) {
            Some(name) => split_trailing_year_preserve_punct(&name),
            None => (String::new(), None),
        };
        if series.is_empty() {
            return None;
        }
        return Some(TvIdentity {
            series,
            year,
            season_number,
            episode_number,
            episode_title: extract_episode_title(suffix),
        });
    }

    None
}

/// Resolve series name + year for the SxxEyy filename path. Prefers a
/// directory-derived series name (preserving punctuation like
/// "Star Trek - Enterprise"); falls back to cleaning the filename
/// prefix only when no usable parent exists.
fn series_and_year(path: &Path, filename_prefix: &str) -> (String, Option<i64>) {
    if let Some(name) = walk_series_dir(path) {
        return split_trailing_year_preserve_punct(&name);
    }
    let trimmed = filename_prefix.trim_end_matches([' ', '.', '_', '-']);
    let cleaned = clean(trimmed);
    if cleaned.is_empty() {
        return (String::new(), None);
    }
    split_trailing_year_clean(&cleaned)
}

/// Strip a trailing `(YYYY)` / `YYYY` from a directory-derived series
/// name without touching internal punctuation. "Severance (2022)" →
/// ("Severance", Some(2022)). "Star Trek - Enterprise" →
/// ("Star Trek - Enterprise", None).
fn split_trailing_year_preserve_punct(name: &str) -> (String, Option<i64>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(?P<name>.+?)[\s._\-]*\(?(?P<year>19\d{2}|20\d{2})\)?\s*$")
            .expect("static dir-year regex")
    });
    let trimmed = name.trim();
    if let Some(caps) = re.captures(trimmed)
        && let (Some(n), Some(y)) = (caps.name("name"), caps.name("year"))
    {
        let mut base = n.as_str().trim().to_string();
        // Drop a trailing open-paren or bracket that the lazy
        // match may have left, plus any stray separator before it.
        while base
            .chars()
            .next_back()
            .map(|c| matches!(c, ' ' | '(' | '[' | '-' | '.' | '_'))
            .unwrap_or(false)
        {
            base.pop();
        }
        if !base.is_empty()
            && let Ok(year) = y.as_str().parse::<i64>()
        {
            return (base, Some(year));
        }
    }
    (trimmed.to_string(), None)
}

/// Same as above but starts from an already-cleaned filename (dots /
/// underscores converted to spaces); used only when the file lives at
/// the library root with no parents.
fn split_trailing_year_clean(cleaned: &str) -> (String, Option<i64>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(?P<name>.+?)\s+\(?(?P<year>19\d{2}|20\d{2})\)?\s*$")
            .expect("static cleaned-year regex")
    });
    if let Some(caps) = re.captures(cleaned)
        && let (Some(n), Some(y)) = (caps.name("name"), caps.name("year"))
        && let Ok(year) = y.as_str().parse::<i64>()
    {
        let base = n.as_str().trim().to_string();
        if !base.is_empty() {
            return (base, Some(year));
        }
    }
    (cleaned.to_string(), None)
}

struct SeasonEpisodeMatch<'a> {
    season_number: i64,
    episode_number: i64,
    prefix: &'a str,
    suffix: &'a str,
}

fn parse_season_episode(stem: &str) -> Option<SeasonEpisodeMatch<'_>> {
    // SxxEyy / Sxx Eyy / Sxx.Eyy / Sxx-Eyy / SxxEPyy. No trailing
    // word-boundary so things like `S01E01E02` and `S02E10WEBRip` match
    // (the suffix gets cleaned up by extract_episode_title).
    static RE_SE: OnceLock<Regex> = OnceLock::new();
    let re_se = RE_SE.get_or_init(|| {
        Regex::new(r"(?i)\bS(?P<s>\d{1,2})[\s._\-]*EP?(?P<e>\d{1,3})").expect("static SxxEyy regex")
    });
    if let Some(caps) = re_se.captures(stem) {
        let m = caps.get(0)?;
        let season_number = caps["s"].parse::<i64>().ok()?;
        let episode_number = caps["e"].parse::<i64>().ok()?;
        return Some(SeasonEpisodeMatch {
            season_number,
            episode_number,
            prefix: &stem[..m.start()],
            suffix: &stem[m.end()..],
        });
    }

    // NxMM — episode requires ≥2 digits so timestamps like 1x3 don't
    // become false positives.
    static RE_X: OnceLock<Regex> = OnceLock::new();
    let re_x = RE_X.get_or_init(|| {
        Regex::new(r"\b(?P<s>\d{1,2})x(?P<e>\d{2,3})\b").expect("static NxMM regex")
    });
    if let Some(caps) = re_x.captures(stem) {
        let m = caps.get(0)?;
        let season_number = caps["s"].parse::<i64>().ok()?;
        let episode_number = caps["e"].parse::<i64>().ok()?;
        return Some(SeasonEpisodeMatch {
            season_number,
            episode_number,
            prefix: &stem[..m.start()],
            suffix: &stem[m.end()..],
        });
    }

    None
}

/// Fallback: parent directory carries the season number (S01 / Season 01
/// / Specials) and the filename starts with `E\d{1,3}`. Returns
/// `(season_number, episode_number, suffix_after_episode)`.
fn parse_from_parent_season<'a>(path: &Path, stem: &'a str) -> Option<(i64, i64, &'a str)> {
    let parent_name = path.parent()?.file_name()?.to_str()?;
    let season_number = parse_season_from_dirname(parent_name)?;

    static RE_E: OnceLock<Regex> = OnceLock::new();
    let re_e = RE_E
        .get_or_init(|| Regex::new(r"(?i)^\s*E(?P<e>\d{1,3})\b").expect("static leading-E regex"));
    let caps = re_e.captures(stem)?;
    let m = caps.get(0)?;
    let episode_number = caps["e"].parse::<i64>().ok()?;
    Some((season_number, episode_number, &stem[m.end()..]))
}

/// Parse a directory name that names a season. Accepts `Season 1`,
/// `Season01`, `S01`, and `Specials` (returns season 0). Returns
/// `None` for anything else.
fn parse_season_from_dirname(name: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?:season\s*(?P<n>\d{1,3})|s(?P<n2>\d{1,2}))\s*$")
            .expect("static season-dir regex")
    });
    if let Some(caps) = re.captures(name) {
        if let Some(n) = caps.name("n").and_then(|m| m.as_str().parse::<i64>().ok()) {
            return Some(n);
        }
        if let Some(n) = caps.name("n2").and_then(|m| m.as_str().parse::<i64>().ok()) {
            return Some(n);
        }
    }
    if name.trim().eq_ignore_ascii_case("specials") {
        return Some(0);
    }
    None
}

/// Walk up the path looking for the first non-junk, non-season-like,
/// non-season-bearing parent directory.
fn walk_series_dir(path: &Path) -> Option<String> {
    let mut current = path.parent();
    while let Some(parent) = current {
        match parent.file_name().and_then(|s| s.to_str()) {
            Some("") => break,
            Some(name) => {
                if is_skippable_dir(name) {
                    current = parent.parent();
                    continue;
                }
                // "Batman S02", "Star Trek TNG S01", "HOC S03": dir
                // contains a season suffix. Strip it and use the rest
                // as the series candidate, but prefer the grandparent
                // if it's a better non-junk name.
                if let Some(stripped) = strip_trailing_season_suffix(name) {
                    if let Some(grand) = parent
                        .parent()
                        .and_then(|gp| gp.file_name())
                        .and_then(|s| s.to_str())
                        && !grand.is_empty()
                        && !is_skippable_dir(grand)
                        && strip_trailing_season_suffix(grand).is_none()
                    {
                        return Some(grand.to_string());
                    }
                    return Some(stripped);
                }
                return Some(name.to_string());
            }
            None => break,
        }
    }
    None
}

fn is_skippable_dir(name: &str) -> bool {
    static RE_SEASON: OnceLock<Regex> = OnceLock::new();
    let re_season = RE_SEASON.get_or_init(|| {
        Regex::new(r"(?i)^\s*(season\s*\d+|s\d{1,2})\s*$").expect("static season-dir regex")
    });
    if re_season.is_match(name) {
        return true;
    }
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("specials") {
        return true;
    }

    // Junk sidecar directories that release groups commonly add next
    // to the real content. Walk past them when looking for the series
    // title so a `Sample/foo.S01E01.mkv` doesn't end up as a series
    // called "Sample".
    static RE_JUNK: OnceLock<Regex> = OnceLock::new();
    let re_junk = RE_JUNK.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(samples?|extras?|bonus(?:es)?|featurettes?|cover|menu|trailers?|intros?|behind[\s._\-]*the[\s._\-]*scenes|bts)\s*$",
        )
        .expect("static junk-dir regex")
    });
    re_junk.is_match(name)
}

/// If a directory name ends with a season suffix (` S02`, `.S03`,
/// `_S01`), return the part before the suffix cleaned up. Used to turn
/// `Batman - The Brave and the Bold` parents that incorrectly include
/// a season into a usable series title.
fn strip_trailing_season_suffix(name: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^(?P<base>.+?)[\s._\-]+s\d{1,2}\b.*$").expect("static season-suffix regex")
    });
    let caps = re.captures(name.trim())?;
    let base = caps.name("base")?.as_str().trim();
    if base.is_empty() {
        return None;
    }
    Some(clean(base))
}

/// Best-effort episode title from the part of the filename stem after
/// the SxxEyy marker. Cuts at the first release-quality / codec tag.
fn extract_episode_title(suffix: &str) -> Option<String> {
    let trimmed = suffix.trim_start_matches([
        ' ', '.', '_', '-', '\u{2014}', /* — */
        '\u{2013}', /* – */
    ]);
    if trimmed.is_empty() {
        return None;
    }

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:2160p|1080p|720p|480p|web[-._]?dl|webrip|bluray|hdtv|dvdrip|remux|x26[45]|h\.?26[45]|hevc|avc|aac|ac3|dts|dts[-._]?hd|flac|dd[\d.]+|ddp[\d.]+|repack|proper|internal|hdr|10bit|ntsc|pal|multi)\b",
        )
        .expect("static quality-marker regex")
    });
    let candidate = match re.find(trimmed) {
        Some(m) => &trimmed[..m.start()],
        None => trimmed,
    };
    let cleaned = clean(candidate);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn id(path: &str) -> Option<TvIdentity> {
        identify_tv(&PathBuf::from(path))
    }

    #[test]
    fn s01e01_with_season_dir() {
        let i = id("Severance/Season 01/Severance.S01E03.1080p.WEB-DL.mkv").unwrap();
        assert_eq!(i.series, "Severance");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 3);
    }

    #[test]
    fn s01e01_flat() {
        let i = id("The.Office.S02E11.1080p.mkv").unwrap();
        assert_eq!(i.series, "The Office");
        assert_eq!(i.season_number, 2);
        assert_eq!(i.episode_number, 11);
    }

    #[test]
    fn series_dir_with_year() {
        let i = id("Severance (2022)/Season 01/Severance.S01E03.mkv").unwrap();
        assert_eq!(i.series, "Severance");
        assert_eq!(i.year, Some(2022));
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 3);
    }

    #[test]
    fn nxmm_pattern() {
        let i = id("Show Name/1x07.mkv").unwrap();
        assert_eq!(i.series, "Show Name");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 7);
    }

    #[test]
    fn nxmm_with_three_digit_episode() {
        let i = id("Anime Show/12x123.mkv").unwrap();
        assert_eq!(i.season_number, 12);
        assert_eq!(i.episode_number, 123);
    }

    #[test]
    fn unsupported_pattern_returns_none() {
        assert!(id("Show Name/Episode 3.mkv").is_none());
    }

    #[test]
    fn nxmm_requires_two_digit_episode() {
        assert!(id("Show/1x3.mkv").is_none());
    }

    #[test]
    fn episode_title_extracted_when_clean() {
        let i = id("Severance/Season 01/Severance.S01E03.In.Perpetuity.1080p.mkv").unwrap();
        assert_eq!(i.episode_title.as_deref(), Some("In Perpetuity"));
    }

    #[test]
    fn episode_title_none_when_only_noise() {
        let i = id("Show/S01E01.1080p.WEB-DL.mkv").unwrap();
        assert!(i.episode_title.is_none());
    }

    #[test]
    fn specials_dir_falls_through_to_series() {
        let i = id("Severance/Specials/Severance.S00E01.mkv").unwrap();
        assert_eq!(i.series, "Severance");
        assert_eq!(i.season_number, 0);
        assert_eq!(i.episode_number, 1);
    }

    #[test]
    fn series_falls_back_to_filename_prefix_when_flat() {
        let i = id("The.Office.S01E01.mkv").unwrap();
        assert_eq!(i.series, "The Office");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 1);
    }

    // New patterns ----------------------------------------------------

    #[test]
    fn multi_episode_takes_first() {
        let i = id("Caprica.S01E01E02.Pilot.mkv").unwrap();
        assert_eq!(i.series, "Caprica");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 1);
    }

    #[test]
    fn space_between_s_and_e() {
        let i = id(
            "Batman - The Brave and the Bold/Batman S02/S02 E01 - SHOOT A CROOKED ARROW 1080p BluRay.mkv",
        )
        .unwrap();
        assert_eq!(i.series, "Batman - The Brave and the Bold");
        assert_eq!(i.season_number, 2);
        assert_eq!(i.episode_number, 1);
        assert_eq!(i.episode_title.as_deref(), Some("SHOOT A CROOKED ARROW"));
    }

    #[test]
    fn dot_between_s_and_e() {
        let i = id("Chernobyl/Chernobyl.S01.E03.2019.2160p.HDR.mkv").unwrap();
        assert_eq!(i.series, "Chernobyl");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 3);
    }

    #[test]
    fn ep_variant() {
        let i = id("House of Cards (US)/HOC S03/House.of.Cards.US.S03EP01.BluRay.10Bit.1080p.mkv")
            .unwrap();
        assert_eq!(i.series, "House of Cards (US)");
        assert_eq!(i.season_number, 3);
        assert_eq!(i.episode_number, 1);
    }

    #[test]
    fn no_separator_between_episode_and_quality_tag() {
        let i = id("Star Trek - Enterprise/S02/Star.Trek.Enterprise.S02E10WEBRip.mkv").unwrap();
        assert_eq!(i.series, "Star Trek - Enterprise");
        assert_eq!(i.season_number, 2);
        assert_eq!(i.episode_number, 10);
    }

    #[test]
    fn parent_season_with_bare_e_filename() {
        let i = id("The Mighty Boosh/S01/E01 - Killeroo.mkv").unwrap();
        assert_eq!(i.series, "The Mighty Boosh");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 1);
        assert_eq!(i.episode_title.as_deref(), Some("Killeroo"));
    }

    #[test]
    fn parent_season_word_with_bare_e_filename() {
        let i = id("Some Show/Season 03/E07 - Title.mkv").unwrap();
        assert_eq!(i.season_number, 3);
        assert_eq!(i.episode_number, 7);
    }

    #[test]
    fn series_parent_with_trailing_sxx_uses_grandparent() {
        let i = id(
            "Star Trek - The Next Generation/Star Trek TNG S01/Star.Trek.The.Next.Generation.S01E03.mkv",
        )
        .unwrap();
        assert_eq!(i.series, "Star Trek - The Next Generation");
        assert_eq!(i.season_number, 1);
        assert_eq!(i.episode_number, 3);
    }

    #[test]
    fn junk_sample_dir_is_skipped() {
        // No SxxEyy in the filename → falls through to None rather than
        // creating a "Sample" series.
        assert!(
            id("Anne of Green Gables/Anne.of.Green.Gables.1985.Part1/Sample/sample.mkv").is_none()
        );
    }

    #[test]
    fn junk_dir_skipped_when_real_episode_exists() {
        let i = id("Severance/Extras/Severance.S01E01.bonus.mkv").unwrap();
        assert_eq!(i.series, "Severance");
        assert_eq!(i.season_number, 1);
    }

    #[test]
    fn strip_trailing_season_returns_clean_name() {
        let s = strip_trailing_season_suffix("HOC S03").unwrap();
        assert_eq!(s, "HOC");
    }

    #[test]
    fn strip_trailing_season_only_when_present() {
        assert!(strip_trailing_season_suffix("Severance").is_none());
    }
}
