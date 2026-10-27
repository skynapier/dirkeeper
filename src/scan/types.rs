use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Core data types for directory classification.
// 1. Kind enum with lowercase wire values and Display.
// 2. Scope enum for classification scope.
// 3. Counts struct with arithmetic methods.
// 4. ScanOptions struct with validation.
// 5. ScanIssue, DirectoryRecord, ScanSummary, ScanError.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Audio,
    Video,
    Software,
    Other,
    Mixed,
    Empty,
    Container,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Image => write!(f, "image"),
            Kind::Audio => write!(f, "audio"),
            Kind::Video => write!(f, "video"),
            Kind::Software => write!(f, "software"),
            Kind::Other => write!(f, "other"),
            Kind::Mixed => write!(f, "mixed"),
            Kind::Empty => write!(f, "empty"),
            Kind::Container => write!(f, "container"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Scope {
    #[default]
    Direct,
    Subtree,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub image: u64,
    pub audio: u64,
    pub video: u64,
    pub software: u64,
    pub other: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.image + self.audio + self.video + self.software + self.other
    }

    pub fn add(&mut self, other: &Self) {
        self.image += other.image;
        self.audio += other.audio;
        self.video += other.video;
        self.software += other.software;
        self.other += other.other;
    }

    pub fn increment(&mut self, kind: Kind) {
        match kind {
            Kind::Image => self.image += 1,
            Kind::Audio => self.audio += 1,
            Kind::Video => self.video += 1,
            Kind::Software => self.software += 1,
            Kind::Other => self.other += 1,
            Kind::Mixed | Kind::Empty | Kind::Container => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub recursive: bool,
    pub max_depth: Option<usize>,
    pub include_hidden: bool,
    pub scope: Scope,
    pub min_ratio: f64,
    pub kinds: Vec<Kind>,
    pub exclude_paths: Vec<PathBuf>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            recursive: false,
            max_depth: None,
            include_hidden: false,
            scope: Scope::Direct,
            min_ratio: 0.5,
            kinds: Vec::new(),
            exclude_paths: Vec::new(),
        }
    }
}

impl ScanOptions {
    /// Depth bound for emitted entries: `max_depth` (or unbounded) when recursive, else `0`.
    pub(crate) fn emitted_limit(&self) -> usize {
        match self.recursive {
            true => self.max_depth.unwrap_or(usize::MAX),
            false => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryRecord {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub depth: usize,
    pub kind: Kind,
    pub counts: Counts,
    pub ratio: f64,
    pub child_dirs: u64,
    pub complete: bool,
    pub issues: Vec<ScanIssue>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanSummary {
    pub directories_scanned: u64,
    pub directories_emitted: u64,
    pub errors: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("invalid options: {0}")]
    InvalidOptions(String),
    #[error("invalid root {path:?}: {message}")]
    InvalidRoot { path: PathBuf, message: String },
    #[error("io error: {0}")]
    Output(#[from] std::io::Error),
}
