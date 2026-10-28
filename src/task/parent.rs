use std::fs;
use std::path::{Path, PathBuf};

use crate::task::item::{MoveError, sync_dir};
use crate::task::model::Stage;
use crate::task::store::TaskStore;

/// Verify that `dir` is an existing, non-symlink, canonical directory.
/// Returns `MoveError::Conflict` for symlink / not-a-directory / not-canonical
/// and `MoveError::Io` for stat / canonicalize errors.
fn check_dir(dir: &Path, label: &str) -> Result<(), MoveError> {
    let meta = fs::symlink_metadata(dir)
        .map_err(|e| MoveError::Io(format!("Cannot stat {} {}: {}", label, dir.display(), e)))?;
    if meta.file_type().is_symlink() {
        return Err(MoveError::Conflict(format!(
            "{} {} is a symlink",
            label,
            dir.display()
        )));
    }
    if !meta.is_dir() {
        return Err(MoveError::Conflict(format!(
            "{} {} is not a directory",
            label,
            dir.display()
        )));
    }
    let canon = dir
        .canonicalize()
        .map_err(|e| MoveError::Io(format!("Cannot canonicalize {}: {}", label, e)))?;
    if canon != dir {
        return Err(MoveError::Conflict(format!(
            "{} {} is not canonical",
            label,
            dir.display()
        )));
    }
    Ok(())
}

/// Ensure the destination's parent directory exists, creating it if the plan
/// marked the move as needing a new parent.
///
/// 1. Do nothing unless `store.plan.moves[index].create_parent` is true.
/// 2. If the parent already exists, verify it is a non-symlink, canonical
///    directory (else `MoveError::Conflict`), then return `Ok(())`.
/// 3. Otherwise verify the grandparent is an existing non-symlink canonical
///    directory (else `MoveError::Conflict`), create the parent with
///    `fs::create_dir`; on `AlreadyExists` re-run step 2's check; other errors
///    become `MoveError::Io`.
/// 4. Fsync the grandparent via `sync_dir`.
/// 5. Journal a `Stage::ParentCreated` event (only when this call created it).
pub(crate) fn ensure_parent(store: &mut TaskStore, index: usize) -> Result<(), MoveError> {
    // 1.
    if !store.plan.moves[index].create_parent {
        return Ok(());
    }

    let dst = PathBuf::from(&store.plan.moves[index].destination);
    let parent = dst
        .parent()
        .ok_or_else(|| MoveError::Io("Destination has no parent".to_string()))?
        .to_path_buf();
    let grandparent = parent
        .parent()
        .ok_or_else(|| MoveError::Io("Parent has no grandparent".to_string()))?
        .to_path_buf();

    // 2.
    if fs::symlink_metadata(&parent).is_ok() {
        check_dir(&parent, "Parent")?;
        return Ok(());
    }

    // 3.
    check_dir(&grandparent, "Grandparent")?;

    match fs::create_dir(&parent) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Re-run step 2's check
            check_dir(&parent, "Parent")?;
            return Ok(());
        }
        Err(e) => {
            return Err(MoveError::Io(format!(
                "Cannot create parent {}: {}",
                parent.display(),
                e
            )));
        }
    }

    // 4.
    sync_dir(&grandparent)?;

    // 5.
    store
        .event(
            index,
            Stage::ParentCreated,
            Some(parent.display().to_string()),
        )
        .map_err(MoveError::Io)?;

    Ok(())
}
