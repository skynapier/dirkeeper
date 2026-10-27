use dirkeeper::scan::{Kind, ScanOptions, Scope, scan};
use std::fs;
use std::path::{Path, PathBuf};

fn collect(root: &std::path::Path, opts: &ScanOptions) -> Vec<dirkeeper::scan::DirectoryRecord> {
    let mut recs = Vec::new();
    scan(root, opts, |r| {
        recs.push(r.clone());
        Ok(())
    })
    .unwrap();
    recs
}

#[test]
fn test_basic_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let unicode_dir = root.join("图片");
    fs::create_dir(&unicode_dir).unwrap();
    fs::write(unicode_dir.join("a.jpg"), b"").unwrap();
    fs::write(unicode_dir.join("b.PNG"), b"").unwrap();
    fs::write(unicode_dir.join("c.txt"), b"").unwrap();
    fs::write(root.join("d.jpg"), b"").unwrap();
    fs::write(root.join("e.txt"), b"").unwrap();

    let opts = ScanOptions {
        recursive: true,
        scope: Scope::Direct,
        ..Default::default()
    };
    let recs = collect(root, &opts);
    assert_eq!(recs.len(), 2);

    let root_rec = recs.iter().find(|r| r.depth == 0).unwrap();
    assert_eq!(root_rec.kind, Kind::Mixed);
    assert_eq!(root_rec.counts.image, 1);
    assert_eq!(root_rec.counts.other, 1);
    assert!(root_rec.path.is_absolute());

    let uni_rec = recs.iter().find(|r| r.depth == 1).unwrap();
    assert_eq!(uni_rec.relative_path, PathBuf::from("图片"));
    assert_eq!(uni_rec.kind, Kind::Image);
    assert_eq!(uni_rec.counts.image, 2);
    assert_eq!(uni_rec.counts.other, 1);
    assert!((uni_rec.ratio - 2.0 / 3.0).abs() < f64::EPSILON);

    let json = serde_json::to_string(uni_rec).unwrap();
    assert!(json.contains("\"kind\":\"image\""));
}

#[test]
fn test_classification_ties() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let d1 = root.join("tie");
    fs::create_dir(&d1).unwrap();
    fs::write(d1.join("a.jpg"), b"").unwrap();
    fs::write(d1.join("b.mp3"), b"").unwrap();
    fs::write(d1.join("c.txt"), b"").unwrap();

    let d2 = root.join("mixed");
    fs::create_dir(&d2).unwrap();
    fs::write(d2.join("a.jpg"), b"").unwrap();
    fs::write(d2.join("b.jpg"), b"").unwrap();
    fs::write(d2.join("c.mp3"), b"").unwrap();
    fs::write(d2.join("d.mp4"), b"").unwrap();

    let d3 = root.join("other");
    fs::create_dir(&d3).unwrap();
    fs::write(d3.join("a.txt"), b"").unwrap();
    fs::write(d3.join("b.txt"), b"").unwrap();
    fs::write(d3.join("c.txt"), b"").unwrap();
    fs::write(d3.join("d.jpg"), b"").unwrap();

    let opts = ScanOptions {
        recursive: true,
        scope: Scope::Direct,
        ..Default::default()
    };
    let recs = collect(root, &opts);

    let r1 = recs
        .iter()
        .find(|r| r.relative_path.as_path() == Path::new("tie"))
        .unwrap();
    assert_eq!(r1.kind, Kind::Mixed);

    let r2 = recs
        .iter()
        .find(|r| r.relative_path.as_path() == Path::new("mixed"))
        .unwrap();
    assert_eq!(r2.kind, Kind::Mixed);

    let r3 = recs
        .iter()
        .find(|r| r.relative_path.as_path() == Path::new("other"))
        .unwrap();
    assert_eq!(r3.kind, Kind::Other);
}

#[test]
fn test_min_ratio_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(root.join("a.jpg"), b"").unwrap();
    fs::write(root.join("b.png"), b"").unwrap();
    fs::write(root.join("c.gif"), b"").unwrap();
    fs::write(root.join("d.txt"), b"").unwrap();

    let opts_75 = ScanOptions {
        recursive: false,
        min_ratio: 0.75,
        ..Default::default()
    };
    let recs_75 = collect(root, &opts_75);
    assert_eq!(recs_75[0].kind, Kind::Mixed);

    let opts_74 = ScanOptions {
        recursive: false,
        min_ratio: 0.74,
        ..Default::default()
    };
    let recs_74 = collect(root, &opts_74);
    assert_eq!(recs_74[0].kind, Kind::Image);
}

#[cfg(unix)]
#[test]
fn test_executable_classification() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    fs::write(root.join("a.mp3"), b"").unwrap();
    fs::write(root.join("b.mp4"), b"").unwrap();
    fs::write(root.join("c.exe"), b"").unwrap();
    fs::write(root.join("d"), b"").unwrap();
    fs::write(root.join("e.jpg"), b"").unwrap();
    fs::write(root.join("f.rar"), b"").unwrap();
    fs::write(root.join("g.pdf"), b"").unwrap();

    let mut perms = fs::metadata(root.join("d")).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(root.join("d"), perms).unwrap();

    let mut perms_e = fs::metadata(root.join("e.jpg")).unwrap().permissions();
    perms_e.set_mode(0o755);
    fs::set_permissions(root.join("e.jpg"), perms_e).unwrap();

    let mut perms_f = fs::metadata(root.join("f.rar")).unwrap().permissions();
    perms_f.set_mode(0o755);
    fs::set_permissions(root.join("f.rar"), perms_f).unwrap();

    let mut perms_g = fs::metadata(root.join("g.pdf")).unwrap().permissions();
    perms_g.set_mode(0o755);
    fs::set_permissions(root.join("g.pdf"), perms_g).unwrap();

    let opts = ScanOptions {
        recursive: false,
        ..Default::default()
    };
    let recs = collect(root, &opts);
    let r = &recs[0];
    assert_eq!(r.counts.audio, 1);
    assert_eq!(r.counts.video, 1);
    assert_eq!(r.counts.software, 2); // c.exe and d
    assert_eq!(r.counts.image, 1); // e.jpg
    assert_eq!(r.counts.other, 2); // f.rar and g.pdf
}
