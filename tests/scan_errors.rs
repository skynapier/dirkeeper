use dirkeeper::scan::{DirectoryRecord, ScanError, ScanOptions, Scope, scan};
use std::io;
use std::path::Path;

fn opts() -> ScanOptions {
    ScanOptions {
        recursive: true,
        max_depth: None,
        include_hidden: false,
        scope: Scope::Direct,
        min_ratio: 0.5,
        kinds: vec![],
        exclude_paths: vec![],
    }
}

fn collect(
    root: &Path,
    options: &ScanOptions,
    mut cb: impl FnMut(&DirectoryRecord) -> io::Result<()>,
) -> Result<Vec<DirectoryRecord>, ScanError> {
    let mut out = Vec::new();
    let r = scan(root, options, |rec| {
        out.push(rec.clone());
        cb(rec)
    });
    match r {
        Ok(_) => Ok(out),
        Err(e) => Err(e),
    }
}

#[test]
fn test_invalid_options_and_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut o = opts();
    for r in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.49, 1.01] {
        o.min_ratio = r;
        let mut called = false;
        let res = scan(root, &o, |_| {
            called = true;
            Ok(())
        });
        assert!(matches!(res, Err(ScanError::InvalidOptions(_))));
        assert!(!called);
    }
    o.min_ratio = 0.5;
    let missing = root.join("missing");
    let res = scan(&missing, &o, |_| Ok(()));
    assert!(matches!(res, Err(ScanError::InvalidRoot { .. })));
    let file = root.join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let res = scan(&file, &o, |_| Ok(()));
    assert!(matches!(res, Err(ScanError::InvalidRoot { .. })));
}

#[test]
fn test_broken_pipe() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for i in 0..3 {
        std::fs::create_dir(root.join(format!("d{i}"))).unwrap();
        std::fs::write(root.join(format!("d{i}/a.txt")), "x").unwrap();
    }
    let mut count = 0usize;
    let res = scan(root, &opts(), |_rec| {
        count += 1;
        if count == 1 {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe"))
        } else {
            Ok(())
        }
    });
    match res {
        Err(ScanError::Output(e)) => assert_eq!(e.kind(), io::ErrorKind::BrokenPipe),
        other => panic!("expected Output(BrokenPipe), got {other:?}"),
    }
    assert_eq!(count, 1);
}

#[cfg(unix)]
#[test]
fn test_symlinks() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("child")).unwrap();
    std::fs::write(root.join("child/a.txt"), "x").unwrap();
    let loopdir = root.join("loopdir");
    symlink(root.join("child"), &loopdir).unwrap();
    let loopfile = root.join("loopfile");
    symlink(root.join("child/a.txt"), &loopfile).unwrap();
    let res = collect(root, &opts(), |_| Ok(()));
    let records = res.unwrap();
    let child = records
        .iter()
        .find(|r| r.relative_path.to_string_lossy() == "child")
        .unwrap();
    assert_eq!(child.child_dirs, 0);
    assert_eq!(child.counts.other, 1);
    assert!(
        records
            .iter()
            .all(|r| r.relative_path.to_string_lossy() != "loopdir")
    );
    assert!(
        records
            .iter()
            .all(|r| r.relative_path.to_string_lossy() != "loopfile")
    );
    let link = root.join("rootlink");
    symlink(root, &link).unwrap();
    let res = scan(&link, &opts(), |_| Ok(()));
    assert!(matches!(res, Err(ScanError::InvalidRoot { .. })));
}

#[cfg(unix)]
#[test]
fn test_unreadable_child() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let child = root.join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("a.txt"), "x").unwrap();
    let sibling = root.join("sibling");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("b.txt"), "x").unwrap();
    let original = std::fs::metadata(&child).unwrap().permissions();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o0)).unwrap();
    if std::fs::read_dir(&child).is_ok() {
        std::fs::set_permissions(&child, original).unwrap();
        return;
    }
    let mut direct_records = Vec::new();
    let direct_res = scan(root, &opts(), |rec| {
        direct_records.push(rec.clone());
        Ok(())
    });
    let mut sub_opts = opts();
    sub_opts.scope = Scope::Subtree;
    let mut sub_records = Vec::new();
    let sub_res = scan(root, &sub_opts, |rec| {
        sub_records.push(rec.clone());
        Ok(())
    });
    std::fs::set_permissions(&child, original).unwrap();
    let direct_summary = direct_res.unwrap();
    let sub_summary = sub_res.unwrap();
    assert!(direct_summary.errors > 0);
    assert!(sub_summary.errors > 0);
    let child_rec = direct_records
        .iter()
        .find(|r| r.relative_path.to_string_lossy() == "child")
        .unwrap();
    assert!(!child_rec.complete);
    assert!(!child_rec.issues.is_empty());
    let sib_rec = direct_records
        .iter()
        .find(|r| r.relative_path.to_string_lossy() == "sibling")
        .unwrap();
    assert!(sib_rec.complete);
    assert!(sib_rec.issues.is_empty());
    let root_rec = direct_records
        .iter()
        .find(|r| r.relative_path.to_string_lossy() == "")
        .unwrap();
    assert!(root_rec.complete);
    assert!(root_rec.issues.is_empty());
    let sub_root = sub_records
        .iter()
        .find(|r| r.relative_path.to_string_lossy() == "")
        .unwrap();
    assert!(!sub_root.complete);
}
