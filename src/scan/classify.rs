use super::types::{Counts, Kind};
use std::path::Path;

// File and directory classification logic.
// 1. classify_file: mime_guess then software extensions/executable flag.
// 2. classify_counts: aggregate counts into Kind with ratio.

const SOFTWARE_EXTS: &[&str] = &[
    "exe", "msi", "com", "bat", "cmd", "ps1", "sh", "jar", "appimage", "apk", "deb", "rpm", "dmg",
    "pkg", "wasm",
];

/// Classifies a single file by mime type, extension, and executable flag.
pub fn classify_file(path: &Path, executable: bool) -> Kind {
    mime_kind(path).unwrap_or_else(|| extension_kind(path, executable))
}

/// Returns the [`Kind`] for a file based on its mime type, if it is an
/// image, audio, or video; otherwise `None`.
fn mime_kind(path: &Path) -> Option<Kind> {
    mime_guess::from_path(path)
        .first()
        .and_then(|m| match m.type_() {
            mime_guess::mime::IMAGE => Some(Kind::Image),
            mime_guess::mime::AUDIO => Some(Kind::Audio),
            mime_guess::mime::VIDEO => Some(Kind::Video),
            _ => None,
        })
}

/// Returns the [`Kind`] for a file based on its extension or executable flag.
fn extension_kind(path: &Path, executable: bool) -> Kind {
    let ext = path.extension().and_then(|e| e.to_str());
    match ext {
        Some(ext) => {
            let ext_lower = ext.to_lowercase();
            match SOFTWARE_EXTS.contains(&ext_lower.as_str()) {
                true => Kind::Software,
                false => Kind::Other,
            }
        }
        None => match executable {
            true => Kind::Software,
            false => Kind::Other,
        },
    }
}

/// Aggregates file counts into a single [`Kind`] with its share of the total.
pub fn classify_counts(counts: &Counts, child_dirs: u64, min_ratio: f64) -> (Kind, f64) {
    // 1. Zero files: Container if child_dirs > 0, else Empty.
    let total = counts.total();
    if total == 0 {
        let kind = match child_dirs > 0 {
            true => Kind::Container,
            false => Kind::Empty,
        };
        return (kind, 0.0);
    }

    // 2. Find unique max count among file kinds.
    let entries: &[(Kind, u64)] = &[
        (Kind::Image, counts.image),
        (Kind::Audio, counts.audio),
        (Kind::Video, counts.video),
        (Kind::Software, counts.software),
        (Kind::Other, counts.other),
    ];
    let (max_count, max_kind, is_unique) = entries.iter().fold(
        (0u64, None, true),
        |(max_count, max_kind, is_unique), (kind, count)| match count.cmp(&max_count) {
            std::cmp::Ordering::Greater => (*count, Some(*kind), true),
            std::cmp::Ordering::Equal if max_count > 0 => (max_count, max_kind, false),
            _ => (max_count, max_kind, is_unique),
        },
    );

    // 3. Compute ratio of max count to total.
    let ratio = max_count as f64 / total as f64;

    // 4. Unique max AND ratio strictly > min_ratio => file Kind; else Mixed.
    let kind = match is_unique && ratio > min_ratio {
        true => max_kind.unwrap(),
        false => Kind::Mixed,
    };
    (kind, ratio)
}
