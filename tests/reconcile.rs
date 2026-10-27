use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dirkeeper"))
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("failed to spawn dirkeeper");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn create_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
}

fn assert_file_exists(path: &Path) {
    assert!(path.exists(), "expected file to exist: {}", path.display());
}

fn assert_dir_exists(path: &Path) {
    assert!(
        path.is_dir(),
        "expected directory to exist: {}",
        path.display()
    );
}

fn assert_dir_not_exists(path: &Path) {
    assert!(
        !path.exists(),
        "expected directory not to exist: {}",
        path.display()
    );
}

fn read_json(path: &Path) -> serde_json::Value {
    let data = fs::read_to_string(path).unwrap();
    serde_json::from_str(&data).unwrap()
}

fn assert_no_staging_or_tmp(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(".staging") || name.ends_with(".tmp") {
            panic!(
                "unexpected staging or tmp file found: {}",
                entry.path().display()
            );
        }
    }
}

#[test]
fn default_existing_destination_stays_conflict_and_source_preserved() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let src = root.join("src");
    let dst = root.join("dst");
    let task_dir = root.join("task");

    create_dir(&src);
    create_dir(&dst);
    write_file(&src.join("a.txt"), "source-a");
    write_file(&dst.join("a.txt"), "dest-a");

    let input = root.join("input.json");
    let spec = serde_json::json!([{
        "source": src.to_str().unwrap(),
        "destination": dst.to_str().unwrap()
    }]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task_dir.to_str().unwrap(),
    ]);

    assert!(
        !ok,
        "expected failure when conflict is not resolved, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["conflicts"], 1);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 0);
    assert_eq!(summary["errors"], 0);

    assert_file_exists(&src.join("a.txt"));
    assert_file_exists(&dst.join("a.txt"));
    assert_dir_exists(&task_dir);

    let plan = read_json(&task_dir.join("plan.json"));
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["moves"][0]["status"], "conflict");
    assert_eq!(plan["moves"][0]["reason"], "Destination exists");
    assert!(plan["moves"][0].get("reconciliation").is_none());
}

#[test]
fn opted_in_duplicate_source_subset_destination_removes_source_preserving_dst_only_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let src = root.join("src");
    let dst = root.join("dst");
    let task_dir = root.join("task");

    create_dir(&src);
    create_dir(&dst);
    write_file(&src.join("a.txt"), "shared");
    write_file(&dst.join("a.txt"), "shared");
    write_file(&dst.join("b.txt"), "dst-only");

    let input = root.join("input.json");
    let spec = serde_json::json!([{
        "source": src.to_str().unwrap(),
        "destination": dst.to_str().unwrap()
    }]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task_dir.to_str().unwrap(),
        "--resolve-existing",
    ]);

    assert!(
        ok,
        "expected success for duplicate reconciliation, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["errors"], 0);

    let plan = read_json(&task_dir.join("plan.json"));
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["moves"][0]["status"], "ready");
    assert_eq!(plan["moves"][0]["reason"], "Reconciled");
    assert_eq!(plan["moves"][0]["reconciliation"]["kind"], "duplicate");
    assert_eq!(
        plan["moves"][0]["reconciliation"]["original_destination"],
        dst.to_str().unwrap()
    );

    let (ok, stdout, stderr) = run(&["apply", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(ok, "expected apply success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);

    assert_dir_not_exists(&src);
    assert_file_exists(&dst.join("a.txt"));
    assert_file_exists(&dst.join("b.txt"));
    assert_no_staging_or_tmp(&root);
}

#[test]
fn merge_equal_intersection_with_source_only_nested_file_empty_file_and_emptydir_yields_union_removes_source_no_staging_tmp_status_resume_success()
 {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let src = root.join("src");
    let dst = root.join("dst");
    let task_dir = root.join("task");

    create_dir(&src);
    create_dir(&dst);

    write_file(&src.join("shared.txt"), "same");
    write_file(&dst.join("shared.txt"), "same");

    write_file(&src.join("nested/page.txt"), "nested");
    write_file(&src.join("empty.txt"), "");
    create_dir(&src.join("emptydir"));

    write_file(&dst.join("dst_only.txt"), "dst-only");

    let input = root.join("input.json");
    let spec = serde_json::json!([{
        "source": src.to_str().unwrap(),
        "destination": dst.to_str().unwrap()
    }]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task_dir.to_str().unwrap(),
        "--resolve-existing",
    ]);

    assert!(
        ok,
        "expected success for merge reconciliation, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["errors"], 0);

    let plan = read_json(&task_dir.join("plan.json"));
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["moves"][0]["status"], "ready");
    assert_eq!(plan["moves"][0]["reason"], "Reconciled");
    assert_eq!(plan["moves"][0]["reconciliation"]["kind"], "merge");
    assert_eq!(
        plan["moves"][0]["reconciliation"]["original_destination"],
        dst.to_str().unwrap()
    );

    let (ok, stdout, stderr) = run(&["apply", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(ok, "expected apply success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);

    assert_dir_not_exists(&src);

    assert_file_exists(&dst.join("shared.txt"));
    assert_eq!(fs::read_to_string(dst.join("shared.txt")).unwrap(), "same");
    assert_file_exists(&dst.join("nested/page.txt"));
    assert_eq!(
        fs::read_to_string(dst.join("nested/page.txt")).unwrap(),
        "nested"
    );
    assert_file_exists(&dst.join("empty.txt"));
    assert_eq!(fs::metadata(dst.join("empty.txt")).unwrap().len(), 0);
    assert_dir_exists(&dst.join("emptydir"));
    assert_file_exists(&dst.join("dst_only.txt"));
    assert_eq!(
        fs::read_to_string(dst.join("dst_only.txt")).unwrap(),
        "dst-only"
    );

    assert_no_staging_or_tmp(&root);

    let (ok, stdout, stderr) = run(&["status", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(ok, "expected status success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);

    let (ok, stdout, stderr) = run(&["resume", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(ok, "expected resume success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);
}

#[test]
fn version_differing_same_path_content_preserves_original_dotted_folder_and_new_version_intact() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let src = root.join("src");
    let dst = root.join("book.v1");
    let task_dir = root.join("task");

    create_dir(&src);
    create_dir(&dst);

    write_file(&src.join("a.txt"), "source-a");
    write_file(&dst.join("a.txt"), "dest-a");

    let input = root.join("input.json");
    let spec = serde_json::json!([{
        "source": src.to_str().unwrap(),
        "destination": dst.to_str().unwrap()
    }]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task_dir.to_str().unwrap(),
        "--resolve-existing",
    ]);

    assert!(
        ok,
        "expected success for version reconciliation, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["errors"], 0);

    let plan = read_json(&task_dir.join("plan.json"));
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["moves"][0]["status"], "ready");
    assert_eq!(plan["moves"][0]["reason"], "Reconciled");
    assert_eq!(plan["moves"][0]["reconciliation"]["kind"], "version");
    assert_eq!(
        plan["moves"][0]["reconciliation"]["original_destination"],
        dst.to_str().unwrap()
    );

    let new_dst_str = plan["moves"][0]["destination"].as_str().unwrap();
    let new_dst = PathBuf::from(new_dst_str);
    assert!(
        new_dst
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("book.v1 [version-"),
        "expected version suffix in destination file_name: {}",
        new_dst_str
    );

    let (ok, stdout, stderr) = run(&["apply", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(ok, "expected apply success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);

    assert_dir_not_exists(&src);

    assert_file_exists(&dst.join("a.txt"));
    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "dest-a");

    assert_file_exists(&new_dst.join("a.txt"));
    assert_eq!(
        fs::read_to_string(new_dst.join("a.txt")).unwrap(),
        "source-a"
    );

    assert_no_staging_or_tmp(&root);
}

#[test]
fn source_changed_after_planning_refuses_apply_preserving_source_and_destination() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let src = root.join("src");
    let dst = root.join("dst");
    let task_dir = root.join("task");

    create_dir(&src);
    create_dir(&dst);

    write_file(&src.join("a.txt"), "source-a");
    write_file(&dst.join("a.txt"), "dest-a");

    let input = root.join("input.json");
    let spec = serde_json::json!([{
        "source": src.to_str().unwrap(),
        "destination": dst.to_str().unwrap()
    }]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task_dir.to_str().unwrap(),
        "--resolve-existing",
    ]);

    assert!(
        ok,
        "expected success for version reconciliation, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 1);
    assert_eq!(summary["errors"], 0);

    let plan = read_json(&task_dir.join("plan.json"));
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["moves"][0]["status"], "ready");
    assert_eq!(plan["moves"][0]["reason"], "Reconciled");
    assert_eq!(plan["moves"][0]["reconciliation"]["kind"], "version");

    write_file(&src.join("a.txt"), "changed-source-a");

    let (ok, stdout, stderr) = run(&["apply", "--task-dir", task_dir.to_str().unwrap()]);

    assert!(
        !ok,
        "expected apply failure when source changed after planning, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["conflicts"], 1);
    assert_eq!(summary["errors"], 0);

    assert_file_exists(&src.join("a.txt"));
    assert_eq!(
        fs::read_to_string(src.join("a.txt")).unwrap(),
        "changed-source-a"
    );

    assert_file_exists(&dst.join("a.txt"));
    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "dest-a");

    assert_no_staging_or_tmp(&root);
}
