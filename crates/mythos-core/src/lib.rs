//! Domain types shared across the Mythos workspace.

pub mod library;
pub mod media;

pub use library::{Library, LibraryKind, NewLibrary};
pub use media::{MediaItem, MediaKind};
