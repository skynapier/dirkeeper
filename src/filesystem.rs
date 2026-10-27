use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::Once;

use blake3::Hasher;
use walkdir::WalkDir;

use crate::task::model::{Manifest, ManifestEntry};

/// Convert a relative path to a valid UTF-8 string, returning an error for
/// non-UTF-8 components instead of silently lossy-converting.
pub(crate) fn rel_to_key(rel: &Path) -> Result<String, String> {
    let mut out = String::new();
    for comp in rel.components() {
        let s = comp
            .as_os_str()
            .to_str()
            .ok_or_else(|| "Non-UTF-8 path component in manifest".to_string())?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(s);
    }
    Ok(out)
}

pub(crate) fn build_manifest(root: &Path) -> Result<Manifest, String> {
    let mut manifest = HashMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;

        if entry.file_type().is_symlink() {
            return Err("Symlink found".to_string());
        }

        if entry.file_type().is_dir() {
            let rel_str = rel_to_key(rel)?;
            if rel_str.is_empty() {
                continue; // Skip root itself
            }
            // Preserve all directories as manifest entries, not only empty ones.
            manifest.insert(rel_str, ManifestEntry::Dir);
        } else if entry.file_type().is_file() {
            let rel_str = rel_to_key(rel)?;
            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            let size = metadata.len();
            let mut hasher = Hasher::new();
            let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
            let mut buf = [0u8; 8192];
            loop {
                let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            let hash = hasher.finalize().to_hex().to_string();
            manifest.insert(rel_str, ManifestEntry::File { size, hash });
        } else {
            return Err("Special file found".to_string());
        }
    }
    Ok(manifest)
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let rel = path.strip_prefix(src).map_err(|e| e.to_string())?;
        let target = dst.join(rel);

        if entry.file_type().is_symlink() {
            return Err("Symlink found".to_string());
        }

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else if entry.file_type().is_file() {
            // Create parent if needed (should exist from dir copy)
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }

            // Copy with create_new
            let mut src_file = fs::File::open(path).map_err(|e| e.to_string())?;
            let mut dst_file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .map_err(|e| e.to_string())?;

            io::copy(&mut src_file, &mut dst_file).map_err(|e| e.to_string())?;
            dst_file.sync_all().map_err(|e| e.to_string())?;
            // Preserve permissions
            let perms = entry.metadata().map_err(|e| e.to_string())?.permissions();
            fs::set_permissions(&target, perms).map_err(|e| e.to_string())?;
        } else {
            return Err("Special file found".to_string());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn atomic_rename(src: &Path, dst: &Path) -> Result<(), String> {
    let src_c =
        std::ffi::CString::new(src.to_str().ok_or("Invalid path")?).map_err(|e| e.to_string())?;
    let dst_c =
        std::ffi::CString::new(dst.to_str().ok_or("Invalid path")?).map_err(|e| e.to_string())?;

    let ret = unsafe { libc::renamex_np(src_c.as_ptr(), dst_c.as_ptr(), libc::RENAME_EXCL) };

    if ret == 0 {
        return Ok(());
    }

    let original = io::Error::last_os_error();
    if original.raw_os_error() != Some(libc::ENOTSUP) {
        return Err(original.to_string());
    }

    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!(
            "dirkeeper: exclusive rename (renamex_np RENAME_EXCL) unsupported on this filesystem; \
             using reserved-empty-dir publication fallback. An interrupted run may leave an \
             empty reservation directory that requires manual review."
        );
    });

    // Fallback: reserve an empty directory at dst, then rename src onto it.
    // POSIX rename rejects a non-empty destination, so a populated target is
    // never overwritten. We never delete or clear any existing dst.
    let src_meta = fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if src_meta.file_type().is_symlink() || !src_meta.is_dir() {
        return Err(format!(
            "atomic_rename fallback: source must be a real directory (not a symlink); original error: {}",
            original
        ));
    }

    match fs::create_dir(dst) {
        Ok(_) => {}
        Err(e) => {
            return Err(format!(
                "atomic_rename fallback: could not reserve empty directory at dst (it may already exist); \
                 original error: {}; no existing content was deleted or overwritten",
                e
            ));
        }
    }

    match fs::rename(src, dst) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "atomic_rename fallback: rename onto reserved empty directory failed: {}; \
             original error: {}; both the source stage and the empty reservation at dst were \
             preserved for manual review; no existing target was removed",
            e, original
        )),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn atomic_rename(src: &Path, dst: &Path) -> Result<(), String> {
    let src_c =
        std::ffi::CString::new(src.to_str().ok_or("Invalid path")?).map_err(|e| e.to_string())?;
    let dst_c =
        std::ffi::CString::new(dst.to_str().ok_or("Invalid path")?).map_err(|e| e.to_string())?;

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

    if ret != 0 {
        Err(io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn atomic_rename(_src: &Path, _dst: &Path) -> Result<(), String> {
    Err("Platform unsupported".to_string())
}

pub(crate) fn cleanup_ancestors(start: &Path, roots: &[PathBuf]) {
    let mut current = Some(start);
    while let Some(dir) = current {
        // Check if under any root
        let under_root = roots.iter().any(|r| {
            if dir == r {
                false // Never remove root itself
            } else {
                dir.strip_prefix(r).is_ok()
            }
        });

        if !under_root {
            break;
        }

        // Try to remove
        match fs::remove_dir(&dir) {
            Ok(_) => {
                current = dir.parent();
            }
            Err(_) => {
                break; // Stop at first non-empty or error
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn atomic_rename_never_overwrites_existing_destination() {
        let base = std::env::temp_dir().join(format!(
            "dirkeeper_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();

        // Non-empty existing destination must not be overwritten.
        let src1 = base.join("src1");
        let dst1 = base.join("dst1");
        fs::create_dir_all(&src1).unwrap();
        fs::write(src1.join("a.txt"), b"payload-a").unwrap();
        fs::create_dir_all(&dst1).unwrap();
        fs::write(dst1.join("keep.txt"), b"keep-me").unwrap();
        assert!(atomic_rename(&src1, &dst1).is_err());
        assert!(
            dst1.join("keep.txt").exists(),
            "existing content must be preserved"
        );
        assert!(src1.exists(), "source must remain on failure");

        // Empty existing destination must not be overwritten either.
        let src2 = base.join("src2");
        let dst2 = base.join("dst2");
        fs::create_dir_all(&src2).unwrap();
        fs::write(src2.join("b.txt"), b"payload-b").unwrap();
        fs::create_dir_all(&dst2).unwrap();
        assert!(atomic_rename(&src2, &dst2).is_err());
        assert!(dst2.exists(), "empty reservation must remain");
        assert!(src2.exists(), "source must remain on failure");

        // Normal absent-destination success preserves file bytes.
        let src3 = base.join("src3");
        let dst3 = base.join("dst3");
        fs::create_dir_all(&src3).unwrap();
        fs::write(src3.join("c.txt"), b"payload-c").unwrap();
        assert!(atomic_rename(&src3, &dst3).is_ok());
        assert_eq!(fs::read(dst3.join("c.txt")).unwrap(), b"payload-c");
        assert!(!src3.exists());

        let _ = fs::remove_dir_all(&base);
    }
}
