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

#[test]
fn create_parents_flag_makes_missing_parent_moves_ready_and_apply_creates_them() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let w1 = root.join("lib/w1");
    let w2 = root.join("lib/w2");
    let new_w1 = root.join("lib/[new]/w1");
    let new_w2 = root.join("lib/[new]/w2");

    create_dir(&w1);
    create_dir(&w2);
    write_file(&w1.join("a.txt"), "w1-a");
    write_file(&w2.join("b.txt"), "w2-b");

    let input = root.join("input.json");
    let spec = serde_json::json!([
        {
            "source": w1.to_str().unwrap(),
            "destination": new_w1.to_str().unwrap()
        },
        {
            "source": w2.to_str().unwrap(),
            "destination": new_w2.to_str().unwrap()
        }
    ]);
    fs::write(&input, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    // Without the flag, both moves conflict because the parent is missing.
    let task = root.join("task");
    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task.to_str().unwrap(),
    ]);

    assert!(
        !ok,
        "expected failure when parent is missing without flag, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["conflicts"], 2);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 0);
    assert_eq!(summary["errors"], 0);

    let plan = read_json(&task.join("plan.json"));
    assert_eq!(plan["moves"][0]["status"], "conflict");
    assert_eq!(plan["moves"][1]["status"], "conflict");

    // With the flag, both moves become ready and apply creates the parents.
    let task2 = root.join("task2");
    let (ok, stdout, stderr) = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task2.to_str().unwrap(),
        "--create-parents",
    ]);

    assert!(
        ok,
        "expected success with --create-parents, stderr: {}",
        stderr
    );

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["pending"], 2);
    assert_eq!(summary["errors"], 0);

    let (ok, stdout, stderr) = run(&["apply", "--task-dir", task2.to_str().unwrap()]);

    assert!(ok, "expected apply success, stderr: {}", stderr);

    let summary: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary["completed"], 2);
    assert_eq!(summary["conflicts"], 0);
    assert_eq!(summary["errors"], 0);

    assert_file_exists(&new_w1.join("a.txt"));
    assert_file_exists(&new_w2.join("b.txt"));
    assert_dir_not_exists(&w1);
    assert_dir_not_exists(&w2);

    let journal = fs::read_to_string(task2.join("journal.jsonl")).unwrap();
    let parent_created_lines: Vec<&str> = journal
        .lines()
        .filter(|line| {
            line.contains("\"stage\":\"parent_created\"")
                || line.contains("\"stage\": \"parent_created\"")
        })
        .collect();
    assert_eq!(
        parent_created_lines.len(),
        1,
        "expected exactly one parent_created journal line, got: {}",
        journal
    );
}
