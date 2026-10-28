use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::PlanMove;

/// Execution stage of a move, serialized as a lowercase string.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Stage {
    Copying,
    Verified,
    Published,
    Deleting,
    Completed,
    Conflict,
    Error,
    Cleaned,
    Renaming,
    Merging,
    #[serde(rename = "parent_created")]
    ParentCreated,
}

impl Stage {
    /// True for the failure stages `Conflict` and `Error`.
    pub(crate) fn is_failure(self) -> bool {
        matches!(self, Stage::Conflict | Stage::Error)
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Stage::Copying => "copying",
            Stage::Verified => "verified",
            Stage::Published => "published",
            Stage::Deleting => "deleting",
            Stage::Completed => "completed",
            Stage::Conflict => "conflict",
            Stage::Error => "error",
            Stage::Cleaned => "cleaned",
            Stage::Renaming => "renaming",
            Stage::Merging => "merging",
            Stage::ParentCreated => "parent_created",
        };
        write!(f, "{}", name)
    }
}

/// A single manifest entry: a directory marker or a file with size and hash.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(from = "(u64, String)", into = "(u64, String)")]
pub(crate) enum ManifestEntry {
    Dir,
    File { size: u64, hash: String },
}

impl From<(u64, String)> for ManifestEntry {
    fn from((size, hash): (u64, String)) -> Self {
        if size == 0 && hash == "dir" {
            ManifestEntry::Dir
        } else {
            ManifestEntry::File { size, hash }
        }
    }
}

impl From<ManifestEntry> for (u64, String) {
    fn from(entry: ManifestEntry) -> Self {
        match entry {
            ManifestEntry::Dir => (0, "dir".to_string()),
            ManifestEntry::File { size, hash } => (size, hash),
        }
    }
}

/// Type alias for the manifest map: relative path -> manifest entry.
pub(crate) type Manifest = HashMap<String, ManifestEntry>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct TaskPlan {
    pub(crate) version: u32,
    pub(crate) id: String,
    pub(crate) cleanup_roots: Vec<PathBuf>,
    pub(crate) moves: Vec<PlanMove>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Evidence {
    pub(crate) task_id: String,
    pub(crate) index: usize,
    pub(crate) source: String,
    pub(crate) destination: String,
    pub(crate) manifest: Manifest,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Event {
    pub(crate) task_id: String,
    pub(crate) index: usize,
    pub(crate) stage: Stage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(crate) struct TaskSummary {
    pub(crate) total: usize,
    pub(crate) completed: usize,
    pub(crate) pending: usize,
    pub(crate) conflicts: usize,
    pub(crate) errors: usize,
}
