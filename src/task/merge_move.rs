use std::fs;

use crate::filesystem::{atomic_rename, build_manifest, cleanup_ancestors, copy_tree};
use crate::task::item::{
    MoveError, check_path_is_dir_or_absent, is_completed, safe_delete, sync_dir,
    sync_dirs_bottom_up,
};
use crate::task::model::{Manifest, ManifestEntry, Stage};
use crate::task::reconcile::expected_manifest;
use crate::task::store::TaskStore;

fn last_meaningful(store: &TaskStore, index: usize) -> Option<Stage> {
    store
        .events
        .iter()
        .rev()
        .find(|e| e.index == index && !e.stage.is_failure())
        .map(|e| e.stage)
}

fn manifest_subset(subset: &Manifest, full: &Manifest) -> bool {
    for (k, v) in subset {
        match full.get(k) {
            Some(fv) if fv == v => {}
            _ => return false,
        }
    }
    true
}

fn depth(s: &str) -> usize {
    s.matches('/').count()
}

pub(crate) fn perform(store: &mut TaskStore, index: usize) -> Result<(), MoveError> {
    let move_ = store.plan.moves[index].clone();
    let rec = match &move_.reconciliation {
        Some(r) => r.clone(),
        None => return Err(MoveError::Io("No reconciliation".to_string())),
    };
    let src = std::path::PathBuf::from(&move_.source);
    let dst = std::path::PathBuf::from(&move_.destination);
    let stage_path = store.staging(index);
    let stage = last_meaningful(store, index);

    check_path_is_dir_or_absent(&src, "source")?;
    check_path_is_dir_or_absent(&dst, "destination")?;
    check_path_is_dir_or_absent(&stage_path, "staging")?;

    let evidence = match store.evidence(index).map_err(MoveError::Io)? {
        Some(ev) => ev,
        None => {
            if src.symlink_metadata().is_err() {
                return Err(MoveError::Io("Source absent but no evidence".to_string()));
            }
            let manifest = build_manifest(&src).map_err(MoveError::Io)?;
            if manifest != rec.source_manifest {
                return Err(MoveError::Conflict(
                    "Source manifest does not match plan".to_string(),
                ));
            }
            let ev = crate::task::model::Evidence {
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

    let expected = expected_manifest(&move_, &evidence.manifest);
    let dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;

    for (k, v) in &rec.destination_manifest {
        match dst_manifest.get(k) {
            Some(dv) if dv == v => {}
            _ => {
                return Err(MoveError::Conflict(
                    "Destination baseline mismatch".to_string(),
                ));
            }
        }
    }
    if !manifest_subset(&dst_manifest, &expected) {
        return Err(MoveError::Conflict(
            "Destination contains unexpected entries".to_string(),
        ));
    }

    let src_exists = src.symlink_metadata().is_ok();
    let stage_exists = stage_path.symlink_metadata().is_ok();

    if stage == Some(Stage::Completed) {
        if !manifest_subset(&dst_manifest, &expected) || !manifest_subset(&expected, &dst_manifest)
        {
            return Err(MoveError::Conflict(
                "Completed: destination manifest mismatch".to_string(),
            ));
        }
        if stage_exists {
            return Err(MoveError::Conflict("Completed: staging exists".to_string()));
        }
        if src_exists {
            return Err(MoveError::Conflict(
                "Completed: source reappeared".to_string(),
            ));
        }
        if let Some(parent) = src.parent() {
            cleanup_ancestors(parent, &store.plan.cleanup_roots);
        }
        return Ok(());
    }

    if stage == Some(Stage::Deleting) {
        if !manifest_subset(&dst_manifest, &expected) || !manifest_subset(&expected, &dst_manifest)
        {
            return Err(MoveError::Conflict(
                "Deleting: destination manifest mismatch".to_string(),
            ));
        }
        if stage_exists {
            return Err(MoveError::Conflict("Deleting: staging exists".to_string()));
        }
        if !src_exists {
            store
                .event(index, Stage::Completed, None)
                .map_err(MoveError::Io)?;
            if let Some(parent) = src.parent() {
                cleanup_ancestors(parent, &store.plan.cleanup_roots);
            }
            return Ok(());
        }
        return safe_delete(store, index, &src, &dst, &evidence);
    }

    if src_exists {
        let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
        if src_manifest != evidence.manifest {
            return Err(MoveError::Conflict("Source manifest mismatch".to_string()));
        }
    } else {
        return Err(MoveError::Conflict(
            "Source absent in active phase".to_string(),
        ));
    }

    if rec.kind == crate::task::reconcile::Kind::Duplicate {
        if !manifest_subset(&dst_manifest, &expected) || !manifest_subset(&expected, &dst_manifest)
        {
            return Err(MoveError::Conflict(
                "Duplicate: destination manifest mismatch".to_string(),
            ));
        }
        return safe_delete(store, index, &src, &dst, &evidence);
    }

    // 1. Merge logic: validate and rebuild existing stage
    if stage_exists {
        if stage != Some(Stage::Merging) {
            return Err(MoveError::Conflict(format!(
                "Staging exists but stage is '{:?}'",
                stage
            )));
        }
        let stage_meta =
            fs::symlink_metadata(&stage_path).map_err(|e| MoveError::Io(e.to_string()))?;
        if stage_meta.file_type().is_symlink() || !stage_meta.is_dir() {
            return Err(MoveError::Conflict(
                "Staging is not a real directory".to_string(),
            ));
        }
        let stage_canon = stage_path
            .canonicalize()
            .map_err(|e| MoveError::Io(e.to_string()))?;
        if stage_canon != stage_path {
            return Err(MoveError::Conflict("Staging is not canonical".to_string()));
        }
        // Rebuild stage
        let _ = build_manifest(&stage_path).map_err(MoveError::Io)?;
        fs::remove_dir_all(&stage_path).map_err(|e| MoveError::Io(e.to_string()))?;
        if let Some(parent) = stage_path.parent() {
            sync_dir(parent)?;
        }
    }

    // 2. Create stage exclusively (parent already exists)
    fs::create_dir(&stage_path).map_err(|e| MoveError::Io(e.to_string()))?;
    if let Some(parent) = stage_path.parent() {
        sync_dir(parent)?;
    }
    store
        .event(index, Stage::Merging, None)
        .map_err(MoveError::Io)?;

    // 3. Copy source into stage and verify
    copy_tree(&src, &stage_path).map_err(MoveError::Io)?;
    let stage_manifest = build_manifest(&stage_path).map_err(MoveError::Io)?;
    if stage_manifest != evidence.manifest {
        return Err(MoveError::Conflict("Staging manifest mismatch".to_string()));
    }
    sync_dirs_bottom_up(&stage_path)?;

    // 4. Recheck source after staging
    let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
    if src_manifest != evidence.manifest {
        return Err(MoveError::Conflict(
            "Source changed during merge".to_string(),
        ));
    }

    // 5. Recapture destination manifest and verify subset relationships
    let dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;
    if !manifest_subset(&rec.destination_manifest, &dst_manifest) {
        return Err(MoveError::Conflict(
            "Destination lost baseline entries".to_string(),
        ));
    }
    if !manifest_subset(&dst_manifest, &expected) {
        return Err(MoveError::Conflict(
            "Destination contains unexpected entries".to_string(),
        ));
    }

    // 6. Publish: compute missing publications
    let mut to_publish: Vec<(String, ManifestEntry)> = Vec::new();
    for (k, v) in &evidence.manifest {
        if !dst_manifest.contains_key(k) {
            to_publish.push((k.clone(), v.clone()));
        }
    }
    to_publish.sort_by(|a, b| depth(&a.0).cmp(&depth(&b.0)));

    // 7. Publish directories first (exclusive create, no create_dir_all)
    for (rel, entry) in &to_publish {
        if matches!(entry, ManifestEntry::Dir) {
            let dst_path = dst.join(rel);
            if let Some(parent) = dst_path.parent() {
                fs::create_dir(&dst_path).map_err(|e| MoveError::Io(e.to_string()))?;
                sync_dir(parent)?;
            } else {
                fs::create_dir(&dst_path).map_err(|e| MoveError::Io(e.to_string()))?;
            }
        }
    }

    // 8. Publish files via atomic_rename from stage to destination
    for (rel, entry) in &to_publish {
        if !matches!(entry, ManifestEntry::Dir) {
            let stage_file = stage_path.join(rel);
            let dst_path = dst.join(rel);
            atomic_rename(&stage_file, &dst_path).map_err(MoveError::Io)?;
            if let Some(parent) = dst_path.parent() {
                sync_dir(parent)?;
            }
            if let Some(parent) = stage_file.parent() {
                sync_dir(parent)?;
            }
        }
    }

    // Verify final
    let final_dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;
    if final_dst_manifest != expected {
        return Err(MoveError::Conflict(
            "Final destination manifest mismatch".to_string(),
        ));
    }
    let final_src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
    if final_src_manifest != evidence.manifest {
        return Err(MoveError::Conflict(
            "Final source manifest mismatch".to_string(),
        ));
    }

    // Cleanup stage
    let _ = build_manifest(&stage_path).map_err(MoveError::Io)?;
    fs::remove_dir_all(&stage_path).map_err(|e| MoveError::Io(e.to_string()))?;
    if let Some(parent) = stage_path.parent() {
        sync_dir(parent)?;
    }

    safe_delete(store, index, &src, &dst, &evidence)
}

pub(crate) fn cleanup_owned(store: &mut TaskStore, index: usize) -> Result<(), MoveError> {
    let move_ = &store.plan.moves[index];
    let rec = match &move_.reconciliation {
        Some(r) => r.clone(),
        None => return Err(MoveError::Io("No reconciliation".to_string())),
    };
    let src = std::path::PathBuf::from(&move_.source);
    let dst = std::path::PathBuf::from(&move_.destination);
    let stage_path = store.staging(index);
    let stage = last_meaningful(store, index);

    if stage == Some(Stage::Completed) {
        let completed = is_completed(store, index).map_err(MoveError::Io)?;
        if !completed {
            return Err(MoveError::Conflict("Cleanup: not completed".to_string()));
        }
        if let Some(parent) = src.parent() {
            cleanup_ancestors(parent, &store.plan.cleanup_roots);
        }
        return Ok(());
    }

    match fs::symlink_metadata(&stage_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(MoveError::Io(e.to_string())),
        Ok(stage_meta) => {
            if stage != Some(Stage::Merging) {
                return Err(MoveError::Conflict(format!(
                    "Cleanup: staging exists but stage is '{:?}'",
                    stage
                )));
            }
            if stage_meta.file_type().is_symlink() || !stage_meta.is_dir() {
                return Err(MoveError::Conflict(
                    "Cleanup: staging is not a real directory".to_string(),
                ));
            }
            let stage_canon = stage_path
                .canonicalize()
                .map_err(|e| MoveError::Io(e.to_string()))?;
            if stage_canon != stage_path {
                return Err(MoveError::Conflict(
                    "Cleanup: staging is not canonical".to_string(),
                ));
            }
            let _ = build_manifest(&stage_path).map_err(MoveError::Io)?;

            let evidence = store
                .evidence(index)
                .map_err(MoveError::Io)?
                .ok_or_else(|| MoveError::Conflict("Cleanup: no evidence".to_string()))?;
            let expected = expected_manifest(move_, &evidence.manifest);
            let src_manifest = build_manifest(&src).map_err(MoveError::Io)?;
            if src_manifest != evidence.manifest {
                return Err(MoveError::Conflict(
                    "Cleanup: source manifest mismatch".to_string(),
                ));
            }
            let dst_manifest = build_manifest(&dst).map_err(MoveError::Io)?;
            if !manifest_subset(&rec.destination_manifest, &dst_manifest)
                || !manifest_subset(&dst_manifest, &expected)
            {
                return Err(MoveError::Conflict(
                    "Cleanup: destination manifest mismatch".to_string(),
                ));
            }

            fs::remove_dir_all(&stage_path).map_err(|e| MoveError::Io(e.to_string()))?;
            if let Some(parent) = stage_path.parent() {
                sync_dir(parent)?;
            }
            store
                .event(index, Stage::Cleaned, None)
                .map_err(MoveError::Io)?;
            Ok(())
        }
    }
}
