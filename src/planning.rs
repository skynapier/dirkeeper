use std::path::{Component, Path, PathBuf};

use crate::model::{MoveSpec, MoveStatus, Plan, PlanMove};

/// Lexical validation of a single path: must be absolute, contain no
/// `ParentDir`/`CurDir` components, and (for sources) not be the filesystem
/// root. Returns `Some(reason)` when the path is invalid.
pub(crate) fn lexical_path_problem(p: &Path, is_source: bool) -> Option<String> {
    if !p.is_absolute() {
        return Some("Path must be absolute".to_string());
    }
    for comp in p.components() {
        match comp {
            Component::ParentDir => return Some("Path must not contain '..'".to_string()),
            Component::CurDir => return Some("Path must not contain '.'".to_string()),
            _ => {}
        }
    }
    if is_source && p == Path::new("/") {
        return Some("Source must not be the filesystem root".to_string());
    }
    None
}

/// Full validation of a single move spec against the filesystem. Returns
/// `Some(reason)` when the move is invalid.
pub(crate) fn validate_move(src: &Path, dst: &Path, create_parents: bool) -> Option<String> {
    if let Some(r) = lexical_path_problem(src, true) {
        return Some(r);
    }
    if let Some(r) = lexical_path_problem(dst, false) {
        return Some(r);
    }
    if src == dst {
        return Some("Source and destination identical".to_string());
    }
    // Source and destination must never overlap, even for a single move.
    if paths_overlap(src, dst) {
        return Some("Source and destination overlap".to_string());
    }
    // Source must be an existing non-symlink directory.
    let src_meta = match src.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return Some("Source does not exist".to_string()),
    };
    if src_meta.file_type().is_symlink() {
        return Some("Source is a symlink".to_string());
    }
    if !src_meta.is_dir() {
        return Some("Source is not a directory".to_string());
    }
    // Destination must not exist, including a dangling symlink.
    if dst.symlink_metadata().is_ok() {
        return Some("Destination exists".to_string());
    }
    // Destination parent must be an existing non-symlink directory.
    let dst_parent = match dst.parent() {
        Some(p) => p,
        None => return Some("Destination has no parent".to_string()),
    };
    if dst_parent.symlink_metadata().is_err() {
        // Parent is absent. If create_parents is enabled, validate the grandparent
        // instead and allow the move (apply will create the parent).
        if create_parents {
            let grandparent = match dst_parent.parent() {
                Some(p) => p,
                None => return Some("Destination parent does not exist".to_string()),
            };
            if let Some(problem) = existing_dir_problem(grandparent, "Destination grandparent") {
                return Some(problem);
            }
            return None;
        }
        return Some("Destination parent does not exist".to_string());
    }
    if let Some(problem) = existing_dir_problem(dst_parent, "Destination parent") {
        return Some(problem);
    }
    // Canonical source must match the requested lexical path, rejecting symlink
    // ancestors.
    let src_canon = match src.canonicalize() {
        Ok(c) => c,
        Err(_) => return Some("Source cannot be canonicalized".to_string()),
    };
    if src_canon != src {
        return Some("Source path is not canonical (symlink ancestor)".to_string());
    }
    None
}

/// Check that a directory exists, is not a symlink, and is canonical.
/// Returns `Some(reason)` if any check fails.
fn existing_dir_problem(dir: &Path, label: &str) -> Option<String> {
    let meta = match dir.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return Some(format!("{} does not exist", label)),
    };
    if meta.file_type().is_symlink() {
        return Some(format!("{} is a symlink", label));
    }
    if !meta.is_dir() {
        return Some(format!("{} is not a directory", label));
    }
    let canon = match dir.canonicalize() {
        Ok(c) => c,
        Err(_) => return Some(format!("{} cannot be canonicalized", label)),
    };
    if canon != dir {
        return Some(format!("{} is not canonical (symlink ancestor)", label));
    }
    None
}

/// True when two paths are identical or one is an ancestor of the other.
pub(crate) fn paths_overlap(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Global validation over a set of moves. Returns a reason vector indexed by
/// move position; `None` means the move is ready.
pub(crate) fn validate_moves(
    moves: &[(PathBuf, PathBuf)],
    create_parents: bool,
) -> Vec<Option<String>> {
    let n = moves.len();
    let mut reasons: Vec<Option<String>> = vec![None; n];

    // Per-move validation.
    for i in 0..n {
        if let Some(r) = validate_move(&moves[i].0, &moves[i].1, create_parents) {
            reasons[i] = Some(r);
        }
    }

    // All-pairs overlap: BOTH members of any duplicate/overlap must conflict.
    // Evaluate every pair regardless of earlier errors; preserve first reason.
    for i in 0..n {
        for j in (i + 1)..n {
            let (si, di) = &moves[i];
            let (sj, dj) = &moves[j];
            let conflict = paths_overlap(si, sj)
                || paths_overlap(si, dj)
                || paths_overlap(di, sj)
                || paths_overlap(di, dj);
            if conflict {
                if reasons[i].is_none() {
                    reasons[i] = Some("Overlaps another move".to_string());
                }
                if reasons[j].is_none() {
                    reasons[j] = Some("Overlaps another move".to_string());
                }
            }
        }
    }

    reasons
}

pub(crate) fn generate_plan(specs: &[MoveSpec], create_parents: bool) -> Plan {
    let moves: Vec<(PathBuf, PathBuf)> = specs
        .iter()
        .map(|s| (PathBuf::from(&s.source), PathBuf::from(&s.destination)))
        .collect();
    let reasons = validate_moves(&moves, create_parents);

    let plan_moves = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let reason = reasons[i].clone();
            let status = if reason.is_some() {
                MoveStatus::Conflict
            } else {
                MoveStatus::Ready
            };
            let dst = PathBuf::from(&spec.destination);
            let dst_parent_absent = dst
                .parent()
                .map(|p| p.symlink_metadata().is_err())
                .unwrap_or(false);
            let create_parent = reason.is_none() && create_parents && dst_parent_absent;
            PlanMove {
                source: spec.source.clone(),
                destination: spec.destination.clone(),
                status,
                reason,
                create_parent,
                reconciliation: None,
            }
        })
        .collect();

    Plan {
        version: 1,
        moves: plan_moves,
    }
}

/// Validate a cleanup root: absolute, existing, canonical, non-symlink
/// directory, not the filesystem root, and containing no `ParentDir`.
pub(crate) fn validate_cleanup_root(root: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("Cleanup root must be absolute".to_string());
    }
    for comp in root.components() {
        if matches!(comp, Component::ParentDir | Component::CurDir) {
            return Err("Cleanup root must not contain '..' or '.'".to_string());
        }
    }
    if root == Path::new("/") {
        return Err("Cleanup root must not be the filesystem root".to_string());
    }
    let meta = root
        .symlink_metadata()
        .map_err(|e| format!("Cleanup root does not exist: {}", e))?;
    if meta.file_type().is_symlink() {
        return Err("Cleanup root is a symlink".to_string());
    }
    if !meta.is_dir() {
        return Err("Cleanup root is not a directory".to_string());
    }
    let canon = root
        .canonicalize()
        .map_err(|e| format!("Cleanup root cannot be canonicalized: {}", e))?;
    if canon != root {
        return Err("Cleanup root is not canonical".to_string());
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_missing_parent_without_flag_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(&tmp).unwrap();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let dst = base.join("new_parent/dest");

        let result = validate_move(&src, &dst, false);
        assert_eq!(
            result,
            Some("Destination parent does not exist".to_string())
        );
    }

    #[test]
    fn test_missing_parent_with_flag_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(&tmp).unwrap();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let dst = base.join("new_parent/dest");

        let result = validate_move(&src, &dst, true);
        assert_eq!(result, None);

        let spec = MoveSpec {
            source: src.to_string_lossy().to_string(),
            destination: dst.to_string_lossy().to_string(),
        };
        let plan = generate_plan(&[spec], true);
        assert_eq!(plan.moves[0].status, MoveStatus::Ready);
        assert!(plan.moves[0].create_parent);
    }

    #[test]
    fn test_missing_grandparent_with_flag_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(&tmp).unwrap();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let dst = base.join("a/b/dest");

        let result = validate_move(&src, &dst, true);
        assert_eq!(
            result,
            Some("Destination grandparent does not exist".to_string())
        );
    }

    #[test]
    fn test_existing_parent_with_flag_ready_no_create() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(&tmp).unwrap();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let parent = base.join("existing_parent");
        fs::create_dir(&parent).unwrap();
        let dst = parent.join("dest");

        let result = validate_move(&src, &dst, true);
        assert_eq!(result, None);

        let spec = MoveSpec {
            source: src.to_string_lossy().to_string(),
            destination: dst.to_string_lossy().to_string(),
        };
        let plan = generate_plan(&[spec], true);
        assert_eq!(plan.moves[0].status, MoveStatus::Ready);
        assert!(!plan.moves[0].create_parent);
    }
}
