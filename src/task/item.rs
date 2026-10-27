use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use walkdir::WalkDir;

use crate::task::fast_move;
use crate::task::merge_move;

use crate::filesystem::{atomic_rename, build_manifest, cleanup_ancestors, copy_tree};
use crate::task::model::{Evidence, Manifest, ManifestEntry, Stage};
use crate::task::reconcile::{Kind, expected_manifest};
use crate::task::store::TaskStore;

#[derive(Debug)]
pub(crate) enum MoveError {
    Conflict(String),
    Io(String),
}

impl From<String> for MoveError {
    fn from(s: String) -> Self {
        MoveError::Io(s)
    }
}

pub(crate) fn perform(store: &mut TaskStore, index: usize) -> Result<(), MoveError> {
    let move_ = store.plan.moves[index].clone();
    let src = PathBuf::from(&move_.source);
    let dst = PathBuf::from(&move_.destination);
    let stage_path = store.staging(index);
    let stage = last_meaningful_stage(store, index);

    // Detect symlink/non-directory for source, dest, stage
    check_path_is_dir_or_absent(&src, "source")?;
    check_path_is_dir_or_absent(&dst, "destination")?;
    check_path_is_dir_or_absent(&stage_path, "staging")?;

    // Route duplicate/merge to merge_move
    if let Some(rec) = &move_.reconciliation {
        if rec.kind == Kind::Duplicate || rec.kind == Kind::Merge {
            return merge_move::perform(store, index);
        }
    }

    // Version source validation BEFORE fast_move::recover
    if move_.reconciliation.is_some() {
        if let Some(rec) = &move_.reconciliation {
            if rec.kind == Kind::Version && src.symlink_metadata().is_ok() {
                let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
                if stage == Some(Stage::Deleting) {
                    if !manifest_subset(&src_manifest, &rec.source_manifest) {
                        return Err(MoveError::Conflict(
                            "Version: source manifest not subset of expected".to_string(),
                        ));
                    }
                } else if src_manifest != rec.source_manifest {
                    return Err(MoveError::Conflict(
                        "Version: source manifest does not match expected".to_string(),
                    ));
                }
            }
        }
    }

    if stage == Some(Stage::Completed) {
        // Require source absent, staging absent, dest matches evidence
        if src.symlink_metadata().is_ok() {
            return Err(MoveError::Conflict(
                "Completed: source still exists".to_string(),
            ));
        }
        if stage_path.symlink_metadata().is_ok() {
            return Err(MoveError::Conflict(
                "Completed: staging still exists".to_string(),
            ));
        }
        let evidence = store
            .evidence(index)
            .map_err(MoveError::Io)?
            .ok_or_else(|| MoveError::Conflict("Completed: no evidence".to_string()))?;
        let expected = expected_manifest(&move_, &evidence.manifest);
        let dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;
        if dst_manifest != expected {
            return Err(MoveError::Conflict(
                "Completed: destination manifest mismatch".to_string(),
            ));
        }
        // Re-run bounded cleanup
        if let Some(parent) = src.parent() {
            cleanup_ancestors(parent, &store.plan.cleanup_roots);
        }
        return Ok(());
    }

    // Attempt fast move recovery for crashed renaming intent
    if fast_move::recover(store, index, &src, &dst, &stage_path)? {
        return Ok(());
    }

    // Check if destination exists
    let dst_exists = dst.symlink_metadata().is_ok();

    if dst_exists {
        // Recovery path: evidence must be present
        let evidence = store
            .evidence(index)
            .map_err(MoveError::Io)?
            .ok_or_else(|| MoveError::Conflict("Destination exists but no evidence".to_string()))?;

        // Stage must be verified/published/deleting
        if stage != Some(Stage::Verified)
            && stage != Some(Stage::Published)
            && stage != Some(Stage::Deleting)
        {
            return Err(MoveError::Conflict(format!(
                "Destination exists but stage is '{}'",
                stage.map(|s| s.to_string()).unwrap_or_default()
            )));
        }

        // Staging must be absent
        if stage_path.symlink_metadata().is_ok() {
            return Err(MoveError::Conflict(
                "Destination exists but staging also exists".to_string(),
            ));
        }

        // Full destination manifest must match expected
        let expected = expected_manifest(&move_, &evidence.manifest);
        let dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;
        if dst_manifest != expected {
            return Err(MoveError::Conflict(
                "Destination manifest does not match evidence".to_string(),
            ));
        }

        let src_exists = src.symlink_metadata().is_ok();
        if !src_exists {
            // Source absent: event completed and cleanup
            store
                .event(index, Stage::Completed, None)
                .map_err(MoveError::Io)?;
            if let Some(parent) = src.parent() {
                cleanup_ancestors(parent, &store.plan.cleanup_roots);
            }
            return Ok(());
        }

        // Source exists: require exact evidence match unless durable prior deleting stage
        let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
        let expected = expected_manifest(&move_, &evidence.manifest);
        if stage == Some(Stage::Deleting) {
            if !manifest_subset(&src_manifest, &expected) {
                return Err(MoveError::Conflict(
                    "Source manifest is not a subset of evidence".to_string(),
                ));
            }
        } else {
            if src_manifest != expected {
                return Err(MoveError::Conflict(
                    "Source manifest does not match evidence".to_string(),
                ));
            }
        }

        // Safe-delete source
        safe_delete(store, index, &src, &dst, &evidence)?;
        return Ok(());
    }

    // Destination absent: source must be real dir
    let src_meta = src
        .symlink_metadata()
        .map_err(|e| MoveError::Io(format!("Cannot stat source: {}", e)))?;
    if src_meta.file_type().is_symlink() {
        return Err(MoveError::Conflict("Source is a symlink".to_string()));
    }
    if !src_meta.is_dir() {
        return Err(MoveError::Conflict("Source is not a directory".to_string()));
    }

    // Get or create evidence
    let evidence = match store.evidence(index).map_err(MoveError::Io)? {
        Some(ev) => ev,
        None => {
            // First time: validate, snapshot, save
            if stage_path.symlink_metadata().is_ok() {
                return Err(MoveError::Conflict(
                    "No evidence but staging exists".to_string(),
                ));
            }
            let manifest = build_manifest(&src).map_err(MoveError::Io)?;
            let ev = Evidence {
                task_id: store.plan.id.clone(),
                index,
                source: move_.source.clone(),
                destination: move_.destination.clone(),
                manifest,
            };
            store.save_evidence(&ev).map_err(MoveError::Io)?;
            ev
        }
    };

    // Current source must exact match evidence
    let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
    let expected = expected_manifest(&move_, &evidence.manifest);
    if src_manifest != expected {
        return Err(MoveError::Conflict(
            "Source manifest does not match evidence".to_string(),
        ));
    }

    // Handle existing staging
    if stage_path.symlink_metadata().is_ok() {
        // Remove ONLY if stage is copying/verified AND canonical nonlink realdir AND source exact expected
        if stage == Some(Stage::Copying) || stage == Some(Stage::Verified) {
            let stage_meta = fs::symlink_metadata(&stage_path)
                .map_err(|e| MoveError::Io(format!("Cannot stat staging: {}", e)))?;
            if !stage_meta.file_type().is_symlink() && stage_meta.is_dir() {
                let stage_canon = stage_path
                    .canonicalize()
                    .map_err(|e| MoveError::Io(format!("Cannot canonicalize staging: {}", e)))?;
                if stage_canon == stage_path {
                    // Source must exact match expected
                    let src_manifest_check = build_manifest(&src).map_err(MoveError::Io)?;
                    if src_manifest_check == expected {
                        fs::remove_dir_all(&stage_path)
                            .map_err(|e| MoveError::Io(format!("Cannot remove staging: {}", e)))?;
                    } else {
                        return Err(MoveError::Conflict(
                            "Staging exists but source changed".to_string(),
                        ));
                    }
                } else {
                    return Err(MoveError::Conflict("Staging is not canonical".to_string()));
                }
            } else {
                return Err(MoveError::Conflict(
                    "Staging is not a real directory".to_string(),
                ));
            }
        } else {
            return Err(MoveError::Conflict(format!(
                "Staging exists but stage is '{}'",
                stage.map(|s| s.to_string()).unwrap_or_default()
            )));
        }
    }

    // Attempt fast move after staging removal and before creating new staging
    if fast_move::try_move(store, index, &src, &dst, &evidence)? {
        return Ok(());
    }

    // Create staging
    let dst_parent = dst
        .parent()
        .ok_or_else(|| MoveError::Io("Destination has no parent".to_string()))?;
    let staging = stage_path.clone();
    fs::create_dir(&staging).map_err(|e| MoveError::Io(format!("Cannot create staging: {}", e)))?;
    store
        .event(index, Stage::Copying, None)
        .map_err(MoveError::Io)?;
    sync_dir(dst_parent)?;

    // Copy tree
    copy_tree(&src, &staging).map_err(MoveError::Io)?;

    // Verify staging manifest
    let staging_manifest = build_manifest(&staging).map_err(MoveError::Io)?;
    if staging_manifest != evidence.manifest {
        return Err(MoveError::Conflict(
            "Staging manifest does not match evidence".to_string(),
        ));
    }

    // Sync staging directories bottom-up
    sync_dirs_bottom_up(&staging)?;

    store
        .event(index, Stage::Verified, None)
        .map_err(MoveError::Io)?;

    // Re-verify source manifest matches evidence before publish
    let src_manifest_pre = build_manifest(&src).map_err(MoveError::Io)?;
    if src_manifest_pre != expected {
        return Err(MoveError::Conflict(
            "Source manifest changed before publish".to_string(),
        ));
    }

    // Atomic rename
    atomic_rename(&staging, &dst).map_err(MoveError::Io)?;
    sync_dir(dst_parent)?;

    store
        .event(index, Stage::Published, None)
        .map_err(MoveError::Io)?;

    // Safe-delete source
    safe_delete(store, index, &src, &dst, &evidence)?;

    Ok(())
}

pub(crate) fn is_completed(store: &TaskStore, index: usize) -> Result<bool, String> {
    let move_ = &store.plan.moves[index];
    let src = PathBuf::from(&move_.source);
    let dst = PathBuf::from(&move_.destination);
    let stage_path = store.staging(index);
    let evidence = match store.evidence(index)? {
        Some(ev) => ev,
        None => return Ok(false),
    };
    let expected = expected_manifest(move_, &evidence.manifest);

    // Source must be absent (dangling link isn't absence)
    match src.symlink_metadata() {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }

    // Stage must be absent
    match stage_path.symlink_metadata() {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }

    // Dest must be real canonical dir
    let dst_meta = match dst.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if dst_meta.file_type().is_symlink() {
        return Ok(false);
    }
    if !dst_meta.is_dir() {
        return Ok(false);
    }
    let dst_canon = dst.canonicalize().map_err(|e| e.to_string())?;
    if dst_canon != dst {
        return Ok(false);
    }

    let dst_manifest = build_manifest(&dst).map_err(|e| e.to_string())?;
    Ok(dst_manifest == expected)
}

pub(crate) fn cleanup_owned(store: &mut TaskStore, index: usize) -> Result<(), MoveError> {
    let move_ = &store.plan.moves[index];
    let src = PathBuf::from(&move_.source);
    let dst = PathBuf::from(&move_.destination);
    let stage_path = store.staging(index);
    let stage = last_meaningful_stage(store, index);

    if stage == Some(Stage::Completed) {
        // Require is_completed then bounded empty ancestors
        let completed = is_completed(store, index).map_err(MoveError::Io)?;
        if !completed {
            return Err(MoveError::Conflict("Cleanup: not completed".to_string()));
        }
        if let Some(parent) = src.parent() {
            cleanup_ancestors(parent, &store.plan.cleanup_roots);
        }
        return Ok(());
    }

    // Route duplicate/merge to merge_move
    if let Some(rec) = &move_.reconciliation {
        if rec.kind == Kind::Duplicate || rec.kind == Kind::Merge {
            return merge_move::cleanup_owned(store, index);
        }
    }

    // For all non-completed phases: if staging absent, cleanup is harmless
    match fs::symlink_metadata(&stage_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(MoveError::Io(format!("Cannot stat staging: {}", e))),
        Ok(stage_meta) => {
            if stage == Some(Stage::Copying) || stage == Some(Stage::Verified) {
                // Dest must be absent
                if dst.symlink_metadata().is_ok() {
                    return Err(MoveError::Conflict(
                        "Cleanup: destination exists".to_string(),
                    ));
                }

                if stage_meta.file_type().is_symlink() || !stage_meta.is_dir() {
                    return Err(MoveError::Conflict(
                        "Cleanup: staging is not a real directory".to_string(),
                    ));
                }
                let stage_canon = stage_path
                    .canonicalize()
                    .map_err(|e| MoveError::Io(format!("Cannot canonicalize staging: {}", e)))?;
                if stage_canon != stage_path {
                    return Err(MoveError::Conflict(
                        "Cleanup: staging is not canonical".to_string(),
                    ));
                }

                // Source must exact match expected
                let evidence = store
                    .evidence(index)
                    .map_err(MoveError::Io)?
                    .ok_or_else(|| MoveError::Conflict("Cleanup: no evidence".to_string()))?;
                let expected = expected_manifest(move_, &evidence.manifest);
                let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
                if src_manifest != expected {
                    return Err(MoveError::Conflict(
                        "Cleanup: source manifest mismatch".to_string(),
                    ));
                }

                // Remove staging
                fs::remove_dir_all(&stage_path)
                    .map_err(|e| MoveError::Io(format!("Cannot remove staging: {}", e)))?;
                if let Some(parent) = stage_path.parent() {
                    sync_dir(parent)?;
                }
                store
                    .event(index, Stage::Cleaned, None)
                    .map_err(MoveError::Io)?;
                return Ok(());
            }

            // Staging exists but unknown phase: conflict preserved
            return Err(MoveError::Conflict(format!(
                "Cleanup: staging exists but stage is '{}'",
                stage.map(|s| s.to_string()).unwrap_or_default()
            )));
        }
    }
}

pub(crate) fn check_path_is_dir_or_absent(path: &Path, name: &str) -> Result<(), MoveError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(MoveError::Conflict(format!("{} is a symlink", name)));
            }
            if !meta.is_dir() {
                return Err(MoveError::Conflict(format!("{} is not a directory", name)));
            }
            let canon = path
                .canonicalize()
                .map_err(|e| MoveError::Io(format!("Cannot canonicalize {}: {}", name, e)))?;
            if canon != path {
                return Err(MoveError::Conflict(format!("{} is not canonical", name)));
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(MoveError::Io(format!("Cannot stat {}: {}", name, e))),
    }
}

fn manifest_subset(subset: &Manifest, full: &Manifest) -> bool {
    for (key, entry) in subset {
        match full.get(key) {
            Some(full_entry) => {
                if entry != full_entry {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

pub(crate) fn safe_delete(
    store: &mut TaskStore,
    index: usize,
    src: &Path,
    dst: &Path,
    evidence: &Evidence,
) -> Result<(), MoveError> {
    // Verify destination full manifest expected
    let move_ = &store.plan.moves[index];
    let expected = expected_manifest(move_, &evidence.manifest);
    let dst_manifest = build_manifest(dst).map_err(MoveError::Io)?;
    if dst_manifest != expected {
        return Err(MoveError::Conflict(
            "Safe delete: destination manifest mismatch".to_string(),
        ));
    }

    // Revalidate source manifest against evidence
    let src_manifest = build_manifest(src).map_err(MoveError::Io)?;
    let stage = last_meaningful_stage(store, index);
    if stage == Some(Stage::Deleting) {
        if !manifest_subset(&src_manifest, &evidence.manifest) {
            return Err(MoveError::Conflict(
                "Safe delete: source manifest not subset of evidence".to_string(),
            ));
        }
    } else {
        if src_manifest != evidence.manifest {
            return Err(MoveError::Conflict(
                "Safe delete: source manifest mismatch".to_string(),
            ));
        }
    }

    // DURABLY event deleting BEFORE first source unlink
    store
        .event(index, Stage::Deleting, None)
        .map_err(MoveError::Io)?;

    // Delete only current manifest entries known expected, deepest first
    let mut entries: Vec<(String, ManifestEntry)> = src_manifest
        .iter()
        .filter(|(k, _)| evidence.manifest.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    for (rel, entry) in &entries {
        let path = src.join(rel);
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| MoveError::Io(format!("Cannot stat {}: {}", rel, e)))?;
        if meta.file_type().is_symlink() {
            return Err(MoveError::Conflict(format!(
                "Safe delete: {} is a symlink",
                rel
            )));
        }
        let (size, hash) = match entry {
            ManifestEntry::Dir => (0u64, "dir".to_string()),
            ManifestEntry::File { size, hash } => (*size, hash.clone()),
        };
        if hash == "dir" {
            if !meta.is_dir() {
                return Err(MoveError::Conflict(format!(
                    "Safe delete: {} expected dir but is not",
                    rel
                )));
            }
        } else if !meta.is_file() {
            return Err(MoveError::Conflict(format!(
                "Safe delete: {} expected file but is not",
                rel
            )));
        }
        if meta.file_type().is_file() {
            if !meta.is_file() {
                return Err(MoveError::Conflict(format!(
                    "Safe delete: {} is not a regular file",
                    rel
                )));
            }
            // Rehash blake3 and size exact expected
            let file_hash = hash_file(&path).map_err(MoveError::Io)?;
            if file_hash != hash {
                return Err(MoveError::Conflict(format!(
                    "Safe delete: {} hash mismatch",
                    rel
                )));
            }
            if meta.len() != size {
                return Err(MoveError::Conflict(format!(
                    "Safe delete: {} size mismatch",
                    rel
                )));
            }
            fs::remove_file(&path)
                .map_err(|e| MoveError::Io(format!("Cannot remove file {}: {}", rel, e)))?;
            if let Some(parent) = path.parent() {
                sync_dir(parent)?;
            }
        } else if meta.file_type().is_dir() {
            fs::remove_dir(&path)
                .map_err(|e| MoveError::Io(format!("Cannot remove dir {}: {}", rel, e)))?;
            if let Some(parent) = path.parent() {
                sync_dir(parent)?;
            }
        } else {
            return Err(MoveError::Conflict(format!(
                "Safe delete: {} is a special file",
                rel
            )));
        }
    }

    // Remove root
    fs::remove_dir(src).map_err(|e| MoveError::Io(format!("Cannot remove source root: {}", e)))?;

    // Fsync modified parents incl source parent after root removed for durability
    if let Some(parent) = src.parent() {
        sync_dir(parent)?;
    }

    store
        .event(index, Stage::Completed, None)
        .map_err(MoveError::Io)?;

    if let Some(parent) = src.parent() {
        cleanup_ancestors(parent, &store.plan.cleanup_roots);
    }

    Ok(())
}

fn last_meaningful_stage(store: &TaskStore, index: usize) -> Option<Stage> {
    store
        .events
        .iter()
        .rev()
        .find(|e| e.index == index && !e.stage.is_failure())
        .map(|e| e.stage)
}

fn hash_file(path: &Path) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("Not a regular file".to_string());
        }
        let mut hasher = Hasher::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = file.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    }
    #[cfg(not(unix))]
    {
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("Not a regular file".to_string());
        }
        let mut hasher = Hasher::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = file.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    }
}

pub(crate) fn sync_dir(path: &Path) -> Result<(), MoveError> {
    let file = fs::File::open(path)
        .map_err(|e| MoveError::Io(format!("Cannot open dir for sync: {}", e)))?;
    if let Err(e) = file.sync_all() {
        #[cfg(target_os = "macos")]
        {
            if e.raw_os_error() == Some(libc::ENOTSUP) {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    eprintln!(
                        "warning: directory fsync unsupported on this filesystem; \
                         file fsync + hash verification retained, power-loss durability limited"
                    );
                });
                return Ok(());
            }
        }
        return Err(MoveError::Io(format!("Cannot sync dir: {}", e)));
    }
    Ok(())
}

pub(crate) fn sync_dirs_bottom_up(root: &Path) -> Result<(), MoveError> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| MoveError::Io(e.to_string()))?;
        if entry.file_type().is_dir() {
            dirs.push(entry.path().to_path_buf());
        }
    }
    dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));
    for dir in dirs {
        sync_dir(&dir)?;
    }
    Ok(())
}
