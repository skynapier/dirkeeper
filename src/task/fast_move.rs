//! Fast same-filesystem directory move with durable crash recovery.
//!
//! 1. `try_move` attempts a native no-replace rename (renameat2/renamex_np)
//!    after durably recording a `renaming` intent.
//! 2. `recover` resumes a crashed `renaming` intent from the journal.
//! 3. Both preserve the existing staged-copy path for cross-filesystem or
//!    unsupported native rename.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::filesystem::{build_manifest, cleanup_ancestors};
use crate::task::item::{MoveError, sync_dir};
use crate::task::model::Stage;
use crate::task::store::TaskStore;

/// 1. Attempt a fast same-filesystem no-overwrite rename.
///
/// Returns `Ok(true)` when the move completed via the fast path, `Ok(false)`
/// when the caller should fall back to the staged copy path.
pub(super) fn try_move(
    store: &mut TaskStore,
    index: usize,
    src: &Path,
    dst: &Path,
    evidence: &crate::task::model::Evidence,
) -> Result<bool, MoveError> {
    // 1.1 Destination must be absent before attempting (syscall enforces too).
    if path_exists(dst)? {
        return Err(MoveError::Conflict(
            "Fast move: destination already exists".to_string(),
        ));
    }

    // 1.2 Durably record the renaming intent BEFORE any syscall.
    store
        .event(index, Stage::Renaming, None)
        .map_err(MoveError::Io)?;

    // 1.3 Attempt the platform native no-replace rename.
    match native_no_replace_rename(src, dst) {
        Ok(()) => {
            // 1.4 Sync source and destination parent dirs for durability.
            if let Some(src_parent) = src.parent() {
                sync_dir(src_parent)?;
            }
            if let Some(dst_parent) = dst.parent() {
                if Some(dst_parent) != src.parent() {
                    sync_dir(dst_parent)?;
                }
            }

            // 1.5 Verify source is truly absent.
            if path_exists(src)? {
                return Err(MoveError::Conflict(
                    "Fast move: source still exists after rename".to_string(),
                ));
            }

            // 1.6 Rehash destination and require exact evidence match.
            let dst_manifest = build_manifest(dst).map_err(MoveError::Io)?;
            if dst_manifest != evidence.manifest {
                return Err(MoveError::Conflict(
                    "Fast move: destination manifest mismatch after rename".to_string(),
                ));
            }

            // 1.7 Append completed event and bounded cleanup.
            store
                .event(index, Stage::Completed, None)
                .map_err(MoveError::Io)?;
            if let Some(parent) = src.parent() {
                cleanup_ancestors(parent, &store.plan.cleanup_roots);
            }
            Ok(true)
        }
        Err(NativeRenameError::Unsupported) => {
            // 1.8 Fallback: caller proceeds to staged copy path.
            Ok(false)
        }
        Err(NativeRenameError::Conflict) => {
            // 1.9 Destination appeared between check and syscall.
            Err(MoveError::Conflict(
                "Fast move: destination exists (EEXIST/ENOTEMPTY)".to_string(),
            ))
        }
        Err(NativeRenameError::Io(e)) => {
            // 1.10 Preserve durable renaming intent; surface IO error.
            Err(MoveError::Io(format!("Fast move rename failed: {}", e)))
        }
    }
}

/// 2. Recover from a crashed `renaming` intent.
///
/// Returns `Ok(true)` when recovery completed the move, `Ok(false)` when the
/// caller should continue with normal recovery logic.
pub(super) fn recover(
    store: &mut TaskStore,
    index: usize,
    src: &Path,
    dst: &Path,
    staging: &Path,
) -> Result<bool, MoveError> {
    // 2.1 Latest meaningful stage for this index, skipping error/conflict.
    let latest_meaningful: Option<Stage> = store
        .events
        .iter()
        .rev()
        .find(|e| e.index == index && !e.stage.is_failure())
        .map(|e| e.stage);

    // 2.2 If no meaningful stage is `renaming`, return false.
    if latest_meaningful != Some(Stage::Renaming) {
        return Ok(false);
    }

    // 2.3 If staging is present, fail conflict (unexpected state).
    if path_exists(staging)? {
        return Err(MoveError::Conflict(
            "Recover: staging present during renaming recovery".to_string(),
        ));
    }

    // 2.4 Determine src/dst presence, distinguishing NotFound from other IO.
    let src_exists = path_exists(src)?;
    let dst_exists = path_exists(dst)?;

    match (src_exists, dst_exists) {
        // 2.5 Both present or both absent: conflict, never delete/overwrite.
        (true, true) | (false, false) => {
            return Err(MoveError::Conflict(
                "Recover: ambiguous rename state (both present or both absent)".to_string(),
            ));
        }
        // 2.6 src present + dst absent: rename did not happen; return false to retry.
        (true, false) => {
            return Ok(false);
        }
        // 2.7 src absent + dst present: rename succeeded; verify and complete.
        (false, true) => {
            // 2.7.1 Require saved evidence.
            let evidence = store
                .evidence(index)
                .map_err(MoveError::Io)?
                .ok_or_else(|| MoveError::Conflict("Recover: no evidence".to_string()))?;

            // 2.7.2 Verify destination manifest equals evidence.
            let dst_manifest = build_manifest(dst).map_err(MoveError::Io)?;
            if dst_manifest != evidence.manifest {
                return Err(MoveError::Conflict(
                    "Recover: destination manifest mismatch".to_string(),
                ));
            }

            // 2.7.3 Sync both distinct parent dirs.
            if let Some(src_parent) = src.parent() {
                sync_dir(src_parent)?;
            }
            if let Some(dst_parent) = dst.parent() {
                if Some(dst_parent) != src.parent() {
                    sync_dir(dst_parent)?;
                }
            }

            // 2.7.4 Append completed event.
            store
                .event(index, Stage::Completed, None)
                .map_err(MoveError::Io)?;

            // 2.7.5 Bounded cleanup.
            if let Some(parent) = src.parent() {
                cleanup_ancestors(parent, &store.plan.cleanup_roots);
            }
            Ok(true)
        }
    }
}

/// 3. Classify a path's existence, distinguishing NotFound from other IO errors.
fn path_exists(path: &Path) -> Result<bool, MoveError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(MoveError::Io(format!(
            "Cannot stat {}: {}",
            path.display(),
            e
        ))),
    }
}

/// 4. Platform-specific native no-replace rename error classification.
enum NativeRenameError {
    /// Cross-filesystem, ENOSYS, ENOTSUP/EOPNOTSUPP: fall back to staged copy.
    Unsupported,
    /// EEXIST or ENOTEMPTY: destination conflict.
    Conflict,
    /// Other IO error.
    Io(io::Error),
}

/// 5. Perform the platform native no-replace rename.
///
/// Linux: `renameat2(AT_FDCWD, src, AT_FDCWD, dst, RENAME_NOREPLACE)`
/// macOS: `renamex_np(src, dst, RENAME_EXCL)`
/// Other platforms: return `Unsupported`.
fn native_no_replace_rename(src: &Path, dst: &Path) -> Result<(), NativeRenameError> {
    // 5.1 Convert paths to CString, rejecting NUL bytes.
    let src_c = to_cstring(src)?;
    let dst_c = to_cstring(dst)?;

    #[cfg(target_os = "linux")]
    {
        // 5.2 Linux: renameat2 with RENAME_NOREPLACE.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_renameat2 as libc::c_long,
                libc::AT_FDCWD as libc::c_long,
                src_c.as_ptr() as libc::c_long,
                libc::AT_FDCWD as libc::c_long,
                dst_c.as_ptr() as libc::c_long,
                libc::RENAME_NOREPLACE as libc::c_long,
            )
        };
        if ret == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        classify_rename_error(err)
    }

    #[cfg(target_os = "macos")]
    {
        // 5.3 macOS: renamex_np with RENAME_EXCL.
        let ret = unsafe { libc::renamex_np(src_c.as_ptr(), dst_c.as_ptr(), libc::RENAME_EXCL) };
        if ret == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        classify_rename_error(err)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // 5.4 Other platforms: unsupported, no unsafe writes.
        let _ = (src_c, dst_c);
        Err(NativeRenameError::Unsupported)
    }
}

/// 6. Convert a Path to CString, rejecting NUL bytes.
fn to_cstring(path: &Path) -> Result<CString, NativeRenameError> {
    match CString::new(path.as_os_str().as_bytes()) {
        Ok(cs) => Ok(cs),
        Err(_) => Err(NativeRenameError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains NUL byte",
        ))),
    }
}

/// 7. Classify a rename syscall error into the appropriate variant.
fn classify_rename_error(err: io::Error) -> Result<(), NativeRenameError> {
    match err.raw_os_error() {
        // 7.1 Cross-filesystem or unsupported syscall: fall back.
        Some(code) if code == libc::EXDEV => Err(NativeRenameError::Unsupported),
        Some(code) if code == libc::ENOSYS => Err(NativeRenameError::Unsupported),
        Some(code) if code == libc::ENOTSUP || code == libc::EOPNOTSUPP => {
            Err(NativeRenameError::Unsupported)
        }
        // 7.2 Destination exists or is non-empty: conflict.
        Some(code) if code == libc::EEXIST || code == libc::ENOTEMPTY => {
            Err(NativeRenameError::Conflict)
        }
        // 7.3 Other errors: preserve typed io::Error.
        _ => Err(NativeRenameError::Io(err)),
    }
}
