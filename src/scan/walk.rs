use super::classify::classify_file;
use super::types::{ScanError, ScanIssue, ScanOptions, Scope};
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) enum WalkEvent {
    Directory {
        path: PathBuf,
        depth: usize,
    },
    File {
        path: PathBuf,
        depth: usize,
        kind: super::types::Kind,
    },
    Issue(ScanIssue),
}

/// Walks a directory tree, emitting [`WalkEvent`]s for directories, files,
/// and scan issues.
///
/// # Steps
///
/// 1. Validate the root: it must exist, not be a symlink, and be a directory.
/// 2. Canonicalize the root and resolve any excluded paths against it.
/// 3. Configure the walker (no link following, sorted, depth-limited for
///    `Scope::Direct`).
/// 4. Iterate entries, skipping hidden and excluded paths, and emit
///    directory, file, and issue events.
///
/// Any `io::Error` returned by the `visit` callback is propagated
/// immediately as a [`ScanError::Output`].
pub(crate) fn walk(
    root: &Path,
    options: &ScanOptions,
    mut visit: impl FnMut(WalkEvent) -> io::Result<()>,
) -> Result<(), ScanError> {
    // 1. Validate the root: it must exist, not be a symlink, and be a directory.
    let canonical_root = validate_root(root)?;

    // 2. Canonicalize the root and resolve any excluded paths against it.
    let excluded = resolve_excluded(&canonical_root, options)?;

    // 3. Configure the walker (no link following, sorted, depth-limited for
    //    Scope::Direct).
    let walker = build_walker(&canonical_root, options);

    // 4. Iterate entries, skipping hidden and excluded paths, and emit
    //    directory, file, and issue events.
    walker
        .into_iter()
        .filter_entry(|entry| keep_entry(entry, options, &excluded))
        .try_for_each(|entry_result| visit_entry(entry_result, &canonical_root, &mut visit))
        .map_err(ScanError::Output)
}

/// Validates the root: it must exist, not be a symlink, and be a directory.
fn validate_root(root: &Path) -> Result<PathBuf, ScanError> {
    let original_root = root.to_path_buf();
    let meta = std::fs::symlink_metadata(root).map_err(|e| ScanError::InvalidRoot {
        path: original_root.clone(),
        message: format!("symlink_metadata failed: {e}"),
    })?;
    let kind = match meta.file_type().is_symlink() {
        true => "root is a symlink",
        false if !meta.is_dir() => "root is not a directory",
        false => {
            return std::fs::canonicalize(root).map_err(|e| ScanError::InvalidRoot {
                path: original_root,
                message: format!("canonicalize failed: {e}"),
            });
        }
    };
    Err(ScanError::InvalidRoot {
        path: original_root,
        message: kind.to_string(),
    })
}

/// Canonicalizes each excluded path and rejects one equal to the root.
fn resolve_excluded(root: &Path, options: &ScanOptions) -> Result<Vec<PathBuf>, ScanError> {
    options
        .exclude_paths
        .iter()
        .map(|p| {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                root.join(p)
            };
            let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
            if canon == *root {
                return Err(ScanError::InvalidOptions(
                    "excluding canonical root is not allowed".to_string(),
                ));
            }
            Ok(canon)
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Builds the walker: no link following, sorted, depth-limited for `Scope::Direct`.
fn build_walker(root: &Path, options: &ScanOptions) -> WalkDir {
    let mut walker = WalkDir::new(root)
        .follow_links(false)
        .follow_root_links(false)
        .sort_by_file_name();
    if options.scope == Scope::Direct {
        walker = walker.max_depth(options.emitted_limit().saturating_add(1));
    }
    walker
}

/// Keeps an entry unless it is hidden (depth > 0, unless `include_hidden`) or excluded.
fn keep_entry(entry: &walkdir::DirEntry, options: &ScanOptions, excluded: &[PathBuf]) -> bool {
    let hidden = !options.include_hidden
        && entry.depth() > 0
        && entry.file_name().to_string_lossy().starts_with('.');
    let excluded_match = excluded
        .iter()
        .any(|ex| entry.path() == ex.as_path() || entry.path().starts_with(ex));
    !(hidden || excluded_match)
}

/// Maps a single walkdir result to the event to visit, or an `io::Error`.
fn visit_entry(
    entry_result: Result<walkdir::DirEntry, walkdir::Error>,
    canonical_root: &Path,
    visit: &mut impl FnMut(WalkEvent) -> io::Result<()>,
) -> io::Result<()> {
    let entry = match entry_result {
        Ok(e) => e,
        Err(e) => {
            let path = e
                .path()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| canonical_root.to_path_buf());
            return visit(WalkEvent::Issue(ScanIssue {
                path,
                message: format!("read_dir error: {e}"),
            }));
        }
    };

    let depth = entry.depth();
    let path = entry.path().to_path_buf();

    match entry.file_type() {
        ft if ft.is_dir() => visit(WalkEvent::Directory { path, depth }),
        ft if ft.is_file() => visit_file(&entry, depth, visit),
        _ => Ok(()),
    }
}

/// Emits a file event, reading metadata and classifying the file.
fn visit_file(
    entry: &walkdir::DirEntry,
    depth: usize,
    visit: &mut impl FnMut(WalkEvent) -> io::Result<()>,
) -> io::Result<()> {
    let path = entry.path().to_path_buf();
    let metadata = match entry.metadata() {
        Ok(m) => m,
        Err(e) => {
            return visit(WalkEvent::Issue(ScanIssue {
                path: path.clone(),
                message: format!("metadata error: {e}"),
            }));
        }
    };

    #[cfg(unix)]
    let executable = (metadata.permissions().mode() & 0o111) != 0;
    #[cfg(not(unix))]
    let executable = false;

    let kind = classify_file(&path, executable);
    visit(WalkEvent::File { path, depth, kind })
}
