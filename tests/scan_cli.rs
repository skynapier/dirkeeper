use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn jsonl_output_two_records() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("file.txt"), "x").unwrap();
    let img_dir = root.path().join("图片");
    std::fs::create_dir_all(&img_dir).unwrap();
    std::fs::write(img_dir.join("img.PNG"), "y").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_dirkeeper"))
        .arg("scan")
        .arg(root.path())
        .arg("-R")
        .arg("--format")
        .arg("jsonl")
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(out.stderr.is_empty());

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);

    let child: dirkeeper::scan::DirectoryRecord = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(child.relative_path, Path::new("图片"));
    assert_eq!(child.kind, dirkeeper::scan::Kind::Image);
    assert_eq!(child.counts.image, 1);
    assert_eq!(child.depth, 1);
    assert!(child.complete);

    let root_rec: dirkeeper::scan::DirectoryRecord = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(root_rec.relative_path, Path::new(""));
    assert_eq!(root_rec.kind, dirkeeper::scan::Kind::Other);
    assert_eq!(root_rec.counts.other, 1);
    assert_eq!(root_rec.depth, 0);
    assert!(root_rec.complete);
}
