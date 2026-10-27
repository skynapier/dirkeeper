use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::filesystem::build_manifest;
use crate::model::{MoveStatus, PlanMove};
use crate::planning::paths_overlap;
use crate::task::model::{Manifest, ManifestEntry};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Duplicate,
    Merge,
    Version,
}

fn sorted_manifest_serialize<S>(manifest: &Manifest, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let sorted: BTreeMap<&String, &ManifestEntry> = manifest.iter().collect();
    sorted.serialize(serializer)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Reconciliation {
    pub(crate) kind: Kind,
    pub(crate) original_destination: String,
    #[serde(serialize_with = "sorted_manifest_serialize")]
    pub(crate) source_manifest: Manifest,
    #[serde(serialize_with = "sorted_manifest_serialize")]
    pub(crate) destination_manifest: Manifest,
    pub(crate) conflicting_paths: Vec<String>,
}

fn lexical_path_problem(path: &Path, is_source: bool) -> Option<String> {
    let s = path.to_str()?;
    if s.contains('\0') {
        return Some("Path contains NUL".to_string());
    }
    if s.is_empty() {
        return Some("Path is empty".to_string());
    }
    if !Path::new(s).is_absolute() {
        return Some("Path is not absolute".to_string());
    }
    for part in s.split('/') {
        if part == "." || part == ".." {
            return Some("Path contains '.' or '..'".to_string());
        }
    }
    if s == "/" {
        return Some("Path is root".to_string());
    }
    let _ = is_source;
    None
}

fn versioned_path(original: &Path, manifest: &Manifest) -> Result<String, String> {
    let file_name = original
        .file_name()
        .ok_or("Original destination has no file name")?
        .to_str()
        .ok_or("Original destination file name is not UTF-8")?;

    let sorted: BTreeMap<&String, &ManifestEntry> = manifest.iter().collect();
    let json = serde_json::to_string(&sorted).map_err(|e| e.to_string())?;
    let hash = blake3::hash(json.as_bytes()).to_hex().to_string();
    let suffix = format!(" [version-{}]", &hash[..12]);

    let max_len = 255usize;
    let suffix_bytes = suffix.len();
    if suffix_bytes >= max_len {
        return Err("Suffix too long".to_string());
    }
    let stem_max = max_len - suffix_bytes;
    let mut truncated_stem = file_name.to_string();
    if truncated_stem.len() > stem_max {
        let mut cut = stem_max;
        while !truncated_stem.is_char_boundary(cut) {
            cut -= 1;
        }
        truncated_stem.truncate(cut);
    }

    let new_name = format!("{}{}", truncated_stem, suffix);
    let parent = original
        .parent()
        .ok_or("Original destination has no parent")?;
    let new_path = parent.join(new_name);
    Ok(new_path
        .to_str()
        .ok_or("New destination is not UTF-8")?
        .to_string())
}

pub(crate) fn resolve_moves(moves: &mut [PlanMove]) -> Result<(), String> {
    let n = moves.len();
    for i in 0..n {
        let is_candidate = moves[i].status == MoveStatus::Conflict
            && moves[i].reason.as_deref() == Some("Destination exists");
        if !is_candidate {
            continue;
        }

        let source = moves[i].source.clone();
        let target = moves[i].destination.clone();

        match resolve_one(&source, &target) {
            Ok((new_destination, reconciliation)) => {
                moves[i].destination = new_destination;
                moves[i].reconciliation = Some(reconciliation);
                moves[i].status = MoveStatus::Ready;
                moves[i].reason = Some("Reconciled".to_string());
            }
            Err(e) => {
                moves[i].status = MoveStatus::Conflict;
                moves[i].reason = Some(e);
                moves[i].reconciliation = None;
            }
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let mut paths_i = vec![
                Path::new(&moves[i].source).to_path_buf(),
                Path::new(&moves[i].destination).to_path_buf(),
            ];
            if let Some(rec) = &moves[i].reconciliation {
                paths_i.push(Path::new(&rec.original_destination).to_path_buf());
            }

            let mut paths_j = vec![
                Path::new(&moves[j].source).to_path_buf(),
                Path::new(&moves[j].destination).to_path_buf(),
            ];
            if let Some(rec) = &moves[j].reconciliation {
                paths_j.push(Path::new(&rec.original_destination).to_path_buf());
            }

            let mut conflict = false;
            for p1 in &paths_i {
                for p2 in &paths_j {
                    if paths_overlap(p1, p2) {
                        conflict = true;
                        break;
                    }
                }
                if conflict {
                    break;
                }
            }

            if conflict {
                moves[i].status = MoveStatus::Conflict;
                moves[i].reason = Some("Overlaps another move".to_string());
                moves[j].status = MoveStatus::Conflict;
                moves[j].reason = Some("Overlaps another move".to_string());
            }
        }
    }

    Ok(())
}

fn resolve_one(source: &str, target: &str) -> Result<(String, Reconciliation), String> {
    let src_path = Path::new(source);
    let dst_path = Path::new(target);

    if let Some(r) = lexical_path_problem(src_path, true) {
        return Err(r);
    }
    if let Some(r) = lexical_path_problem(dst_path, false) {
        return Err(r);
    }
    if paths_overlap(src_path, dst_path) {
        return Err("Source and destination overlap".to_string());
    }

    let src_meta = src_path
        .symlink_metadata()
        .map_err(|_| "Source does not exist".to_string())?;
    if src_meta.file_type().is_symlink() {
        return Err("Source is a symlink".to_string());
    }
    if !src_meta.is_dir() {
        return Err("Source is not a directory".to_string());
    }

    let dst_meta = dst_path
        .symlink_metadata()
        .map_err(|_| "Destination does not exist".to_string())?;
    if dst_meta.file_type().is_symlink() {
        return Err("Destination is a symlink".to_string());
    }
    if !dst_meta.is_dir() {
        return Err("Destination is not a directory".to_string());
    }

    let src_canon = src_path
        .canonicalize()
        .map_err(|_| "Source cannot be canonicalized".to_string())?;
    if src_canon != src_path {
        return Err("Source path is not canonical (symlink ancestor)".to_string());
    }

    let dst_canon = dst_path
        .canonicalize()
        .map_err(|_| "Destination cannot be canonicalized".to_string())?;
    if dst_canon != dst_path {
        return Err("Destination path is not canonical (symlink ancestor)".to_string());
    }

    let src_manifest = build_manifest(src_path)?;
    let dst_manifest = build_manifest(dst_path)?;

    let mut conflicting_paths = Vec::new();
    for (k, v) in &src_manifest {
        if let Some(dst_v) = dst_manifest.get(k) {
            if v != dst_v {
                conflicting_paths.push(k.clone());
            }
        }
    }
    conflicting_paths.sort();

    let is_subset = src_manifest.keys().all(|k| dst_manifest.contains_key(k));

    let kind = if !conflicting_paths.is_empty() {
        Kind::Version
    } else if is_subset {
        Kind::Duplicate
    } else {
        Kind::Merge
    };

    let new_destination = match kind {
        Kind::Version => {
            let new_path = versioned_path(dst_path, &src_manifest)?;
            match Path::new(&new_path).symlink_metadata() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("New destination already exists".to_string()),
            }
            new_path
        }
        _ => target.to_string(),
    };

    Ok((
        new_destination,
        Reconciliation {
            kind,
            original_destination: target.to_string(),
            source_manifest: src_manifest,
            destination_manifest: dst_manifest,
            conflicting_paths,
        },
    ))
}

pub(crate) fn expected_manifest(move_: &PlanMove, source: &Manifest) -> Manifest {
    let rec = match &move_.reconciliation {
        Some(r) => r,
        None => return source.clone(),
    };
    match rec.kind {
        Kind::Duplicate | Kind::Merge => {
            let mut m = rec.destination_manifest.clone();
            for (k, v) in source {
                m.insert(k.clone(), v.clone());
            }
            m
        }
        Kind::Version => source.clone(),
    }
}

pub(crate) fn validate_resolution(move_: &PlanMove) -> Result<(), String> {
    let rec = match &move_.reconciliation {
        Some(r) => r,
        None => return Ok(()),
    };

    let src_path = Path::new(&move_.source);
    let orig_path = Path::new(&rec.original_destination);
    let dst_path = Path::new(&move_.destination);

    if let Some(r) = lexical_path_problem(src_path, true) {
        return Err(r);
    }
    if let Some(r) = lexical_path_problem(orig_path, false) {
        return Err(r);
    }
    if let Some(r) = lexical_path_problem(dst_path, false) {
        return Err(r);
    }

    if paths_overlap(src_path, orig_path) {
        return Err("Source and original destination overlap".to_string());
    }
    if rec.kind == Kind::Version {
        if paths_overlap(src_path, dst_path) {
            return Err("Source and destination overlap".to_string());
        }
        if paths_overlap(orig_path, dst_path) {
            return Err("Original destination and destination overlap".to_string());
        }
    }

    for (k, v) in &rec.source_manifest {
        if let Some(r) = validate_key(k) {
            return Err(r);
        }
        match v {
            ManifestEntry::Dir => {}
            ManifestEntry::File { hash, .. } if !is_valid_hash(hash) => {
                return Err(format!("Key {} has invalid hash", k));
            }
            ManifestEntry::File { .. } => {}
        }
        if !has_ancestors(k, &rec.source_manifest) {
            return Err(format!("Key {} missing ancestors", k));
        }
    }

    for (k, v) in &rec.destination_manifest {
        if let Some(r) = validate_key(k) {
            return Err(r);
        }
        match v {
            ManifestEntry::Dir => {}
            ManifestEntry::File { hash, .. } if !is_valid_hash(hash) => {
                return Err(format!("Key {} has invalid hash", k));
            }
            ManifestEntry::File { .. } => {}
        }
        if !has_ancestors(k, &rec.destination_manifest) {
            return Err(format!("Key {} missing ancestors", k));
        }
    }

    let mut conflicting_paths = Vec::new();
    for (k, v) in &rec.source_manifest {
        if let Some(dst_v) = rec.destination_manifest.get(k) {
            if v != dst_v {
                conflicting_paths.push(k.clone());
            }
        }
    }
    conflicting_paths.sort();

    if conflicting_paths != rec.conflicting_paths {
        return Err("Conflicting paths mismatch".to_string());
    }

    let is_subset = rec
        .source_manifest
        .keys()
        .all(|k| rec.destination_manifest.contains_key(k));

    let expected_kind = if !conflicting_paths.is_empty() {
        Kind::Version
    } else if is_subset {
        Kind::Duplicate
    } else {
        Kind::Merge
    };

    if rec.kind != expected_kind {
        return Err("Kind mismatch".to_string());
    }

    match rec.kind {
        Kind::Duplicate | Kind::Merge => {
            if move_.destination != rec.original_destination {
                return Err("Destination must equal original for Duplicate/Merge".to_string());
            }
        }
        Kind::Version => {
            if rec.conflicting_paths.is_empty() {
                return Err("Version requires conflicting paths".to_string());
            }
            let expected = versioned_path(orig_path, &rec.source_manifest)?;
            if expected != move_.destination {
                return Err("Destination mismatch for Version".to_string());
            }
        }
    }

    Ok(())
}

fn validate_key(k: &str) -> Option<String> {
    if k.is_empty() {
        return Some("Key is empty".to_string());
    }
    if k.starts_with('/') {
        return Some("Key has leading slash".to_string());
    }
    if k.contains('\0') {
        return Some("Key contains NUL".to_string());
    }
    for part in k.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Some("Key contains empty segment, '.' or '..'".to_string());
        }
    }
    None
}

fn is_valid_hash(h: &str) -> bool {
    if h.len() != 64 {
        return false;
    }
    h.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn has_ancestors(key: &str, manifest: &Manifest) -> bool {
    let mut current = key;
    loop {
        match current.rfind('/') {
            Some(idx) => {
                current = &current[..idx];
                match manifest.get(current) {
                    Some(ManifestEntry::Dir) => {}
                    _ => return false,
                }
            }
            None => break,
        }
    }
    true
}
