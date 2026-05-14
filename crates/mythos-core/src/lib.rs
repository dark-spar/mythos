//! Domain types shared across the Mythos workspace.

pub mod activity;
pub mod library;
pub mod media;
pub mod movie;
pub mod profile;
pub mod subtitle;
pub mod tv;

pub use activity::{ContinueWatchingEpisode, ContinueWatchingItem, ContinueWatchingMovie};
pub use library::{Library, LibraryKind, NewLibrary};
pub use media::{MediaItem, MediaKind};
pub use movie::{MediaFile, Movie, NewMediaFile, NewMovie, Probe, WatchProgress, sort_title};
pub use profile::{
    AudioCodecCap, ClientProfile, MediaCapabilities, PlaybackMode, PlaybackPlan, VideoCodecCap,
    decide,
};
pub use subtitle::{NewSubtitle, SubtitleTrack, is_image_subtitle_codec};
pub use tv::{Episode, EpisodeProgress, NewEpisode, NewSeason, NewSeries, Season, Series};
