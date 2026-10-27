use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dirkeeper"))
}

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .arg("scan")
        .args(args)
        .output()
        .expect("failed to execute dirkeeper scan")
}

fn parse_json(path: &Path) -> Vec<dirkeeper::scan::DirectoryRecord> {
    let content = fs::read_to_string(path).expect("failed to read report");
    serde_json::from_str(&content).expect("failed to parse JSON")
}

#[test]
fn test_success_overwrite_and_idempotent() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path().join("root");
    fs::create_dir_all(&root).expect("failed to create root");
    let report = root.join("report.json");

    let out = run_cli(&[root.to_str().unwrap(), "-o", report.to_str().unwrap()]);
    assert!(out.status.success(), "expected success, got: {:?}", out);
    assert!(out.stdout.is_empty(), "expected empty stdout");

    let records = parse_json(&report);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, dirkeeper::scan::Kind::Empty);
    assert_eq!(records[0].counts.total(), 0);

    // Rerun same root/report -> same empty
    let out2 = run_cli(&[root.to_str().unwrap(), "-o", report.to_str().unwrap()]);
    assert!(out2.status.success(), "expected success on rerun");
    assert!(out2.stdout.is_empty());
    let records2 = parse_json(&report);
    assert_eq!(records2.len(), 1);
    assert_eq!(records2[0].kind, dirkeeper::scan::Kind::Empty);
    assert_eq!(records2[0].counts.total(), 0);

    // Root read_dir contains only report.json
    let entries: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["report.json"]);
}

#[test]
fn test_invalid_root_and_nan_ratio() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let sentinel = tmp.path().join("sentinel.txt");
    fs::write(&sentinel, "sentinel-data").expect("failed to write sentinel");
    let sentinel_bytes = fs::read(&sentinel).expect("failed to read sentinel");

    // 1. Nonexistent root with -o report must fail/no stdout/sentinel unchanged
    let missing_root = tmp.path().join("missing-root");
    let out = run_cli(&[
        missing_root.to_str().unwrap(),
        "-o",
        sentinel.to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "expected failure for nonexistent root"
    );
    assert!(out.stdout.is_empty(), "expected empty stdout");
    let after_bytes = fs::read(&sentinel).expect("failed to read sentinel after");
    assert_eq!(sentinel_bytes, after_bytes, "sentinel should be unchanged");

    // 2. SEPARATELY create valid root; --min-ratio NaN with -o same sentinel fails/no stdout/unchanged
    let valid_root = tmp.path().join("valid-root");
    fs::create_dir_all(&valid_root).expect("failed to create valid root");
    let out2 = run_cli(&[
        valid_root.to_str().unwrap(),
        "--min-ratio",
        "NaN",
        "-o",
        sentinel.to_str().unwrap(),
    ]);
    assert!(!out2.status.success(), "expected failure for NaN ratio");
    assert!(out2.stdout.is_empty(), "expected empty stdout");
    let after_bytes2 = fs::read(&sentinel).expect("failed to read sentinel after");
    assert_eq!(sentinel_bytes, after_bytes2, "sentinel should be unchanged");
}

#[test]
fn test_missing_parent_and_dir_output() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path().join("root");
    fs::create_dir_all(&root).expect("failed to create root");

    // 1. -o tmp/missing-parent/report.json fails, missing-parent still absent
    let missing_parent = tmp.path().join("missing-parent");
    let report = missing_parent.join("report.json");
    let out = run_cli(&[root.to_str().unwrap(), "-o", report.to_str().unwrap()]);
    assert!(!out.status.success(), "expected failure for missing parent");
    assert!(
        !missing_parent.exists(),
        "missing-parent should still be absent"
    );

    // 2. -o existing dir fails and dir unchanged
    let existing_dir = tmp.path().join("existing-dir");
    fs::create_dir_all(&existing_dir).expect("failed to create existing dir");
    let out2 = run_cli(&[root.to_str().unwrap(), "-o", existing_dir.to_str().unwrap()]);
    assert!(
        !out2.status.success(),
        "expected failure for existing dir output"
    );
    assert!(existing_dir.is_dir(), "existing dir should still be a dir");

    #[cfg(unix)]
    {
        // 3. Output symlink -> sentinel target fails/target bytes unchanged
        let sentinel = tmp.path().join("sentinel.txt");
        fs::write(&sentinel, "target-bytes").expect("failed to write sentinel");
        let target_bytes = fs::read(&sentinel).expect("failed to read sentinel");
        let symlink_path = tmp.path().join("symlink-report.json");
        std::os::unix::fs::symlink(&sentinel, &symlink_path).expect("failed to create symlink");
        let out3 = run_cli(&[root.to_str().unwrap(), "-o", symlink_path.to_str().unwrap()]);
        assert!(
            !out3.status.success(),
            "expected failure for symlink output"
        );
        let after_bytes = fs::read(&sentinel).expect("failed to read sentinel after");
        assert_eq!(
            target_bytes, after_bytes,
            "target bytes should be unchanged"
        );

        // 4. Symlink root refused
        let real_root = tmp.path().join("real-root");
        fs::create_dir_all(&real_root).expect("failed to create real root");
        let symlink_root = tmp.path().join("symlink-root");
        std::os::unix::fs::symlink(&real_root, &symlink_root)
            .expect("failed to create symlink root");
        let out4 = run_cli(&[symlink_root.to_str().unwrap()]);
        assert!(!out4.status.success(), "expected failure for symlink root");
    }
}
