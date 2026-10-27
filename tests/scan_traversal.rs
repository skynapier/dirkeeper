use dirkeeper::scan::{Kind, ScanOptions, Scope, scan};
use std::fs;
use std::path::Path;

fn mk(p: &Path, c: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, c).unwrap();
}

fn d(p: &Path) {
    fs::create_dir_all(p).unwrap();
}

fn collect(root: &Path, opts: &ScanOptions) -> Vec<dirkeeper::scan::DirectoryRecord> {
    let mut v = Vec::new();
    scan(root, opts, |r| {
        v.push(r.clone());
        Ok(())
    })
    .unwrap();
    v
}

fn sel<'a>(
    recs: &'a [dirkeeper::scan::DirectoryRecord],
    rel: &str,
) -> Option<&'a dirkeeper::scan::DirectoryRecord> {
    recs.iter()
        .find(|r| r.relative_path.to_string_lossy() == rel)
}

#[test]
fn direct_recursive_scan() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    mk(&r.join("root.txt"), "a");
    mk(&r.join("child/a.jpg"), "b");
    let o = ScanOptions {
        recursive: true,
        ..Default::default()
    };
    let recs = collect(r, &o);
    let root = sel(&recs, "").unwrap();
    assert_eq!(root.kind, Kind::Other);
    assert_eq!(root.counts.other, 1);
    let child = sel(&recs, "child").unwrap();
    assert_eq!(child.kind, Kind::Image);
    assert_eq!(child.counts.image, 1);
    let o2 = ScanOptions {
        recursive: true,
        scope: Scope::Subtree,
        ..Default::default()
    };
    let recs2 = collect(r, &o2);
    let root2 = sel(&recs2, "").unwrap();
    assert_eq!(root2.kind, Kind::Mixed);
    assert_eq!(root2.counts.image, 1);
    assert_eq!(root2.counts.other, 1);
    assert_eq!(recs2.len(), 2);
    assert_eq!(recs2.last().unwrap().relative_path, Path::new(""));
}

#[test]
fn empty_vs_container() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    d(&r.join("c1"));
    mk(&r.join("c1/a.jpg"), "b");
    mk(&r.join("c1/grandchild/b.mp3"), "c");
    d(&r.join("c2"));
    let o = ScanOptions {
        recursive: false,
        ..Default::default()
    };
    let recs = collect(r, &o);
    let root = sel(&recs, "").unwrap();
    assert_eq!(root.kind, Kind::Container);
    assert_eq!(root.child_dirs, 2);
    mk(&r.join("root.txt"), "a");
    let o2 = ScanOptions {
        recursive: false,
        ..Default::default()
    };
    let recs2 = collect(r, &o2);
    let root2 = sel(&recs2, "").unwrap();
    assert_eq!(root2.kind, Kind::Other);
    assert_eq!(root2.child_dirs, 2);
    let o3 = ScanOptions {
        recursive: true,
        max_depth: Some(0),
        ..Default::default()
    };
    let recs3 = collect(r, &o3);
    assert_eq!(recs3.len(), 1);
    assert_eq!(recs3[0].kind, Kind::Other);
    let o4 = ScanOptions {
        recursive: true,
        max_depth: Some(1),
        ..Default::default()
    };
    let recs4 = collect(r, &o4);
    assert_eq!(recs4.len(), 3);
    let c1 = sel(&recs4, "c1").unwrap();
    assert_eq!(c1.kind, Kind::Image);
    assert_eq!(c1.counts.image, 1);
    assert_eq!(c1.child_dirs, 1);
    assert!(sel(&recs4, "c1/grandchild").is_none());
    let c2 = sel(&recs4, "c2").unwrap();
    assert_eq!(c2.kind, Kind::Empty);
    let o5 = ScanOptions {
        recursive: true,
        scope: Scope::Subtree,
        max_depth: Some(0),
        ..Default::default()
    };
    let recs5 = collect(r, &o5);
    let root5 = sel(&recs5, "").unwrap();
    assert_eq!(root5.kind, Kind::Mixed);
    assert_eq!(root5.counts.other, 1);
    assert_eq!(root5.counts.image, 1);
    assert_eq!(root5.counts.audio, 1);
}

#[test]
fn hidden_prune() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    mk(&r.join("root.txt"), "a");
    mk(&r.join(".hidden.txt"), "b");
    d(&r.join(".hdir"));
    mk(&r.join(".hdir/f.txt"), "c");
    let o = ScanOptions {
        recursive: true,
        ..Default::default()
    };
    let recs = collect(r, &o);
    assert!(sel(&recs, ".hdir").is_none());
    let root = sel(&recs, "").unwrap();
    assert_eq!(root.counts.other, 1);
    let o2 = ScanOptions {
        recursive: true,
        include_hidden: true,
        ..Default::default()
    };
    let recs2 = collect(r, &o2);
    assert!(sel(&recs2, ".hdir").is_some());
    let root2 = sel(&recs2, "").unwrap();
    assert_eq!(root2.counts.other, 2);
    let r2 = td.path().join(".hidden_root");
    d(&r2);
    mk(&r2.join("f.txt"), "x");
    let recs3 = collect(&r2, &o);
    assert!(sel(&recs3, "").is_some());
    let recs4 = collect(&r2, &o2);
    assert!(sel(&recs4, "").is_some());
}

#[test]
fn exclude_paths() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    mk(&r.join("root.txt"), "a");
    mk(&r.join("excl/a.txt"), "b");
    mk(&r.join("excl/sub/b.txt"), "c");
    mk(&r.join("keep.txt"), "d");
    mk(&r.join("excl2.txt"), "e");
    d(&r.join("excl2"));
    mk(&r.join("excl2/f.txt"), "f");
    let o = ScanOptions {
        recursive: true,
        exclude_paths: vec![r.join("excl"), r.join("keep.txt")],
        ..Default::default()
    };
    let recs = collect(r, &o);
    assert!(sel(&recs, "excl").is_none());
    assert!(sel(&recs, "excl/sub").is_none());
    assert!(sel(&recs, "keep.txt").is_none());
    let root = sel(&recs, "").unwrap();
    assert_eq!(root.counts.other, 2);
    assert_eq!(root.child_dirs, 1);
    assert!(sel(&recs, "excl2").is_some());
}

#[test]
fn image_filter_unicode() {
    let td = tempfile::tempdir().unwrap();
    let r = td.path();
    mk(&r.join("root.txt"), "a");
    mk(&r.join("c1/a.jpg"), "b");
    mk(&r.join("c2/b.mp3"), "c");
    mk(&r.join("c3/c.txt"), "d");
    mk(&r.join("c4/d.png"), "e");
    mk(&r.join("c5/é.txt"), "f");
    mk(&r.join("c6/child/h.jpg"), "h");
    let o = ScanOptions {
        recursive: true,
        kinds: vec![Kind::Image],
        ..Default::default()
    };
    let recs = collect(r, &o);
    assert_eq!(recs.len(), 3);
    let c1 = sel(&recs, "c1").unwrap();
    assert_eq!(c1.kind, Kind::Image);
    let c4 = sel(&recs, "c4").unwrap();
    assert_eq!(c4.kind, Kind::Image);
    let c6 = sel(&recs, "c6/child").unwrap();
    assert_eq!(c6.kind, Kind::Image);
    let o2 = ScanOptions {
        recursive: true,
        kinds: vec![Kind::Image, Kind::Audio],
        ..Default::default()
    };
    let recs2 = collect(r, &o2);
    assert_eq!(recs2.len(), 4);
    assert!(sel(&recs2, "c2").is_some());
    let o3 = ScanOptions {
        recursive: true,
        ..Default::default()
    };
    let recs3 = collect(r, &o3);
    assert_eq!(recs3.len(), 8);
    assert_eq!(recs3.last().unwrap().relative_path, Path::new(""));
    let c5 = sel(&recs3, "c5").unwrap();
    assert_eq!(c5.kind, Kind::Other);
}
