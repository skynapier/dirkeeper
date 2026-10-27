// Directory classifier: counts files per directory by type and classifies each directory.
// 1. Private modules aggregate, classify, types, walk; crate-visible cli.
// 2. Public reexports of the library API.

mod aggregate;
mod classify;
pub(crate) mod cli;
mod types;
mod walk;

pub use aggregate::scan;
pub use classify::{classify_counts, classify_file};
pub use types::{
    Counts, DirectoryRecord, Kind, ScanError, ScanIssue, ScanOptions, ScanSummary, Scope,
};
