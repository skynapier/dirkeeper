use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dirkeeper"))
        .args(args)
        .output()
        .expect("failed to execute")
}

fn reset_stage(task_dir: &Path, stage: &str) {
    let plan_path = task_dir.join("plan.json");
    let plan_str = fs::read_to_string(&plan_path).expect("read plan");
    let plan_val: serde_json::Value = serde_json::from_str(&plan_str).expect("parse plan");
    let id = plan_val["id"].as_str().expect("id").to_string();
    let journal = serde_json::json!({
        "task_id": id,
        "index": 0,
        "stage": stage,
        "reason": null
    });
    fs::write(task_dir.join("journal.jsonl"), format!("{}\n", journal)).expect("write journal");
}

fn setup_fixture() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let src = root.join("src").join("book");
    let dest_parent = root.join("dest");
    let target = dest_parent.join("book");
    let task = root.join("task");

    fs::create_dir_all(&src).expect("src");
    fs::create_dir_all(&dest_parent).expect("dest_parent");
    fs::write(src.join("a"), "A").expect("a");
    fs::write(src.join("b"), "B").expect("b");

    let input = root.join("input.json");
    let input_json = serde_json::json!([
        { "source": src.to_string_lossy(), "destination": target.to_string_lossy() }
    ]);
    fs::write(&input, input_json.to_string()).expect("input");

    let plan_out = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task.to_str().unwrap(),
    ]);
    assert!(
        plan_out.status.success(),
        "plan failed: {}",
        String::from_utf8_lossy(&plan_out.stderr)
    );

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(
        apply_out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    (tmp, src, target, task, root)
}

#[test]
fn test_published_before_log_recovery() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    // Restore source files
    fs::create_dir_all(&src).expect("recreate src");
    fs::write(src.join("a"), "A").expect("restore a");
    fs::write(src.join("b"), "B").expect("restore b");

    // Test 1: verified stage
    reset_stage(&task, "verified");
    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "resume verified failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse summary");
    assert_eq!(summary["completed"].as_u64(), Some(1));
    assert!(!src.exists(), "source should be absent");
    assert_eq!(fs::read_to_string(target.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(target.join("b")).unwrap(), "B");

    // Test 1b: deleting stage (partial delete)
    // Re-setup for second part of test 1
    let (_tmp2, src2, target2, task2, _root2) = setup_fixture();
    fs::create_dir_all(&src2).expect("recreate src2");
    fs::write(src2.join("a"), "A").expect("restore a2");
    fs::write(src2.join("b"), "B").expect("restore b2");
    fs::remove_file(src2.join("a")).expect("remove a2");

    reset_stage(&task2, "deleting");
    let out2 = run(&["resume", "--task-dir", task2.to_str().unwrap()]);
    assert!(
        out2.status.success(),
        "resume deleting failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let summary2: serde_json::Value = serde_json::from_slice(&out2.stdout).expect("parse summary2");
    assert_eq!(summary2["completed"].as_u64(), Some(1));
    assert!(!src2.exists(), "source2 should be absent");
    assert_eq!(fs::read_to_string(target2.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(target2.join("b")).unwrap(), "B");
}

#[test]
fn test_changed_source_conflict() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    // Restore source files
    fs::create_dir_all(&src).expect("recreate src");
    fs::write(src.join("a"), "A").expect("restore a");
    fs::write(src.join("b"), "B").expect("restore b");

    // Add unexpected file
    fs::write(src.join("unexpected.txt"), "X").expect("unexpected");

    reset_stage(&task, "deleting");

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!out.status.success(), "should conflict");
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));

    // Verify preservation
    assert!(src.exists(), "source should be preserved");
    assert!(
        src.join("unexpected.txt").exists(),
        "unexpected should be preserved"
    );
    assert_eq!(fs::read_to_string(target.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(target.join("b")).unwrap(), "B");

    // Repeat resume
    let out2 = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!out2.status.success(), "should still conflict");
    let summary2: serde_json::Value = serde_json::from_slice(&out2.stdout).expect("parse summary2");
    assert_eq!(summary2["conflicts"].as_u64(), Some(1));
    assert!(src.exists(), "source should still be preserved");
    assert!(
        src.join("unexpected.txt").exists(),
        "unexpected should still be preserved"
    );
    assert_eq!(fs::read_to_string(target.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(target.join("b")).unwrap(), "B");
}
