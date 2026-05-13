use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryKind {
    Movies,
    Shows,
    Music,
    Photos,
    Books,
}

impl LibraryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LibraryKind::Movies => "movies",
            LibraryKind::Shows => "shows",
            LibraryKind::Music => "music",
            LibraryKind::Photos => "photos",
            LibraryKind::Books => "books",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "movies" => LibraryKind::Movies,
            "shows" => LibraryKind::Shows,
            "music" => LibraryKind::Music,
            "photos" => LibraryKind::Photos,
            "books" => LibraryKind::Books,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub id: Uuid,
    pub name: String,
    pub kind: LibraryKind,
    pub root_path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewLibrary {
    pub name: String,
    pub kind: LibraryKind,
    pub root_path: PathBuf,
}
