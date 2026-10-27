use std::path::{Path, PathBuf};

use crate::model::MoveStatus;
use crate::planning::{lexical_path_problem, paths_overlap, validate_cleanup_root};
use crate::task::store::TaskStore;

pub(crate) fn validate_task(store: &TaskStore) -> Result<(), String> {
    let task_dir = &store.dir;
    let evidence_dir = task_dir.join("evidence");

    // Validate evidence directory
    let ev_meta = std::fs::symlink_metadata(&evidence_dir)
        .map_err(|e| format!("Evidence dir missing: {}", e))?;
    if ev_meta.file_type().is_symlink() {
        return Err("Evidence dir is a symlink".to_string());
    }
    if !ev_meta.is_dir() {
        return Err("Evidence dir is not a directory".to_string());
    }
    let ev_canon = evidence_dir
        .canonicalize()
        .map_err(|e| format!("Evidence dir cannot be canonicalized: {}", e))?;
    if ev_canon != evidence_dir {
        return Err("Evidence dir is not canonical".to_string());
    }

    let moves = &store.plan.moves;
    let n = moves.len();

    // Collect ready moves
    let mut ready_indices: Vec<usize> = Vec::new();
    for i in 0..n {
        match moves[i].status {
            MoveStatus::Ready => {
                ready_indices.push(i);
            }
            MoveStatus::Conflict => {}
        }
    }

    // Validate each ready move
    for &i in &ready_indices {
        let src = PathBuf::from(&moves[i].source);
        let dst = PathBuf::from(&moves[i].destination);

        // Lexical validation
        if let Some(r) = lexical_path_problem(&src, true) {
            return Err(format!("Move {}: {}", i, r));
        }
        if let Some(r) = lexical_path_problem(&dst, false) {
            return Err(format!("Move {}: {}", i, r));
        }
        if src == dst {
            return Err(format!("Move {}: source and destination identical", i));
        }
        if paths_overlap(&src, &dst) {
            return Err(format!("Move {}: source and destination overlap", i));
        }

        // Source validation
        match std::fs::symlink_metadata(&src) {
            Ok(src_meta) => {
                if src_meta.file_type().is_symlink() {
                    return Err(format!("Move {}: source is a symlink", i));
                }
                if !src_meta.is_dir() {
                    return Err(format!("Move {}: source is not a directory", i));
                }
                let src_canon = src
                    .canonicalize()
                    .map_err(|e| format!("Move {}: cannot canonicalize source: {}", i, e))?;
                if src_canon != src {
                    return Err(format!("Move {}: source is not canonical", i));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Source absent: validate nearest existing ancestor
                validate_ancestors(&src)?;
            }
            Err(e) => return Err(format!("Move {}: cannot stat source: {}", i, e)),
        }

        // Destination validation
        if let Some(r) = lexical_path_problem(&dst, true) {
            return Err(format!("Move {}: {}", i, r));
        }
        if dst == Path::new("/") {
            return Err(format!("Move {}: destination is filesystem root", i));
        }
        match std::fs::symlink_metadata(&dst) {
            Ok(dst_meta) => {
                if dst_meta.file_type().is_symlink() {
                    return Err(format!("Move {}: destination is a symlink", i));
                }
                if !dst_meta.is_dir() {
                    return Err(format!("Move {}: destination is not a directory", i));
                }
                let dst_canon = dst
                    .canonicalize()
                    .map_err(|e| format!("Move {}: cannot canonicalize destination: {}", i, e))?;
                if dst_canon != dst {
                    return Err(format!("Move {}: destination is not canonical", i));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Destination absent: validate parent
            }
            Err(e) => return Err(format!("Move {}: cannot stat destination: {}", i, e)),
        }
        // Validate dest parent every row
        let dst_parent = dst
            .parent()
            .ok_or_else(|| format!("Move {}: destination has no parent", i))?;
        let parent_meta = std::fs::symlink_metadata(dst_parent)
            .map_err(|e| format!("Move {}: cannot stat dest parent: {}", i, e))?;
        if parent_meta.file_type().is_symlink() {
            return Err(format!("Move {}: dest parent is a symlink", i));
        }
        if !parent_meta.is_dir() {
            return Err(format!("Move {}: dest parent is not a directory", i));
        }
        let parent_canon = dst_parent
            .canonicalize()
            .map_err(|e| format!("Move {}: cannot canonicalize dest parent: {}", i, e))?;
        if parent_canon != dst_parent {
            return Err(format!("Move {}: dest parent is not canonical", i));
        }
    }

    // Cross-move overlap validation
    for i in 0..ready_indices.len() {
        for j in (i + 1)..ready_indices.len() {
            let idx_i = ready_indices[i];
            let idx_j = ready_indices[j];
            let (si, di) = (&moves[idx_i].source, &moves[idx_i].destination);
            let (sj, dj) = (&moves[idx_j].source, &moves[idx_j].destination);
            let si_p = Path::new(si);
            let di_p = Path::new(di);
            let sj_p = Path::new(sj);
            let dj_p = Path::new(dj);
            if paths_overlap(si_p, sj_p)
                || paths_overlap(si_p, dj_p)
                || paths_overlap(di_p, sj_p)
                || paths_overlap(di_p, dj_p)
            {
                return Err(format!("Moves {} and {} overlap", idx_i, idx_j));
            }
        }
    }

    // Cleanup roots validation
    let roots = &store.plan.cleanup_roots;
    for root in roots {
        let _ = validate_cleanup_root(root)
            .map_err(|e| format!("Cleanup root {}: {}", root.display(), e))?;
    }

    // Reject overlap between cleanup roots and any destination
    for root in roots {
        for &i in &ready_indices {
            let dst = Path::new(&moves[i].destination);
            if paths_overlap(root, dst) {
                return Err(format!(
                    "Cleanup root {} overlaps destination of move {}",
                    root.display(),
                    i
                ));
            }
        }
    }

    // Reject overlap between task dir and source/dest/cleanup
    for &i in &ready_indices {
        let src = Path::new(&moves[i].source);
        let dst = Path::new(&moves[i].destination);
        if paths_overlap(task_dir, src) {
            return Err(format!("Task dir overlaps source of move {}", i));
        }
        if paths_overlap(task_dir, dst) {
            return Err(format!("Task dir overlaps destination of move {}", i));
        }
    }
    for root in roots {
        if paths_overlap(task_dir, root) {
            return Err(format!("Task dir overlaps cleanup root {}", root.display()));
        }
    }

    Ok(())
}

fn validate_ancestors(path: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(format!("Ancestor {} is a symlink", current.display()));
                }
                if !meta.is_dir() {
                    return Err(format!("Ancestor {} is not a directory", current.display()));
                }
                let canon = current
                    .canonicalize()
                    .map_err(|e| format!("Cannot canonicalize {}: {}", current.display(), e))?;
                if canon != current {
                    return Err(format!("Ancestor {} is not canonical", current.display()));
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if current == Path::new("/") {
                    return Err(format!("No existing ancestor found for {}", path.display()));
                }
                current = current
                    .parent()
                    .ok_or_else(|| "Path has no parent".to_string())?
                    .to_path_buf();
            }
            Err(e) => {
                return Err(format!("Cannot stat ancestor {}: {}", current.display(), e));
            }
        }
    }
}
