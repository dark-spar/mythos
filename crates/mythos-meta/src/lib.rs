//! External metadata providers.
//!
//! Phase 1d ships only the TMDb client for movies; MusicBrainz and
//! OpenLibrary clients live behind the same trait shape when their
//! phases arrive.

pub mod tmdb;

pub use tmdb::{TmdbClient, TmdbConfig, TmdbError, TmdbMatch};
