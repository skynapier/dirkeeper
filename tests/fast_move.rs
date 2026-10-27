use std::fs;
use std::os::unix::fs::MetadataExt;
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

fn append_event(task_dir: &Path, stage: &str) {
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
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(task_dir.join("journal.jsonl"))
        .expect("open journal");
    use std::io::Write;
    file.write_all(format!("{}\n", journal).as_bytes())
        .expect("append journal");
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
    fs::create_dir_all(src.join("nested")).expect("nested");

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

    (tmp, src, target, task, root)
}

#[test]
fn test_samefs_direct_move() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    let src_meta = fs::metadata(&src).expect("src meta");
    let src_inode = src_meta.ino();
    let file_a_meta = fs::metadata(src.join("a")).expect("file a meta");
    let file_a_inode = file_a_meta.ino();
    let nested_meta = fs::metadata(src.join("nested")).expect("nested meta");
    let nested_inode = nested_meta.ino();

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(
        apply_out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    assert!(!src.exists(), "source should be absent");
    assert!(target.exists(), "target should exist");
    let staging_entries: Vec<String> = fs::read_dir(target.parent().unwrap())
        .expect("read dest parent")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|name| name.starts_with(".dirplace-") && name.ends_with(".staging"))
        .collect();
    assert!(
        staging_entries.is_empty(),
        "no staging directory in dest parent"
    );

    let dst_meta = fs::metadata(&target).expect("dst meta");
    assert_eq!(dst_meta.ino(), src_inode, "inode preserved");
    let dst_file_a_meta = fs::metadata(target.join("a")).expect("dst file a meta");
    assert_eq!(
        dst_file_a_meta.ino(),
        file_a_inode,
        "file a inode preserved"
    );
    let dst_nested_meta = fs::metadata(target.join("nested")).expect("dst nested meta");
    assert_eq!(
        dst_nested_meta.ino(),
        nested_inode,
        "nested inode preserved"
    );

    let journal_str = fs::read_to_string(task.join("journal.jsonl")).expect("read journal");
    let stages: Vec<String> = journal_str
        .lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            v["stage"].as_str().map(|s| s.to_string())
        })
        .collect();

    assert!(
        stages.iter().any(|s| s == "renaming"),
        "journal has renaming"
    );
    assert!(
        stages.iter().any(|s| s == "completed"),
        "journal has completed"
    );
    assert!(!stages.iter().any(|s| s == "copying"), "journal no copying");
}

#[test]
fn test_crash_recovery() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(
        apply_out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    let target_a = fs::read_to_string(target.join("a")).expect("target a");
    let target_b = fs::read_to_string(target.join("b")).expect("target b");

    reset_stage(&task, "renaming");
    append_event(&task, "error");

    let resume_out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        resume_out.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resume_out.stderr)
    );
    let summary: serde_json::Value =
        serde_json::from_slice(&resume_out.stdout).expect("parse summary");
    assert_eq!(summary["completed"].as_u64(), Some(1));
    assert!(!src.exists(), "source absent");
    assert_eq!(fs::read_to_string(target.join("a")).unwrap(), target_a);
    assert_eq!(fs::read_to_string(target.join("b")).unwrap(), target_b);

    let resume_out2 = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        resume_out2.status.success(),
        "second resume failed: {}",
        String::from_utf8_lossy(&resume_out2.stderr)
    );
    let summary2: serde_json::Value =
        serde_json::from_slice(&resume_out2.stdout).expect("parse summary2");
    assert_eq!(summary2["completed"].as_u64(), Some(1));
}

#[test]
fn test_ambiguous_crash_state() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(
        apply_out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    fs::create_dir_all(&src).expect("recreate src");
    fs::write(src.join("a"), "A").expect("restore a");
    fs::write(src.join("b"), "B").expect("restore b");
    fs::create_dir_all(src.join("nested")).expect("restore nested");

    reset_stage(&task, "renaming");

    let resume_out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!resume_out.status.success(), "should conflict");
    let summary: serde_json::Value =
        serde_json::from_slice(&resume_out.stdout).expect("parse summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));
    assert!(src.exists(), "source preserved");
    assert!(target.exists(), "target preserved");

    let resume_out2 = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!resume_out2.status.success(), "should still conflict");
    let summary2: serde_json::Value =
        serde_json::from_slice(&resume_out2.stdout).expect("parse summary2");
    assert_eq!(summary2["conflicts"].as_u64(), Some(1));
    assert!(src.exists(), "source still preserved");
    assert!(target.exists(), "target still preserved");
}

#[test]
fn test_changed_destination_after_rename() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(
        apply_out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply_out.stderr)
    );

    fs::write(target.join("a"), "CHANGED").expect("change a");

    reset_stage(&task, "renaming");

    let resume_out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!resume_out.status.success(), "should conflict");
    let summary: serde_json::Value =
        serde_json::from_slice(&resume_out.stdout).expect("parse summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));
    assert!(!src.exists(), "source absent");
    assert_eq!(fs::read_to_string(target.join("a")).unwrap(), "CHANGED");
}

#[test]
fn test_preexisting_empty_destination() {
    let (_tmp, src, target, task, _root) = setup_fixture();

    fs::create_dir_all(&target).expect("create empty target");

    let apply_out = run(&["apply", "--task-dir", task.to_str().unwrap()]);
    assert!(!apply_out.status.success(), "should conflict");
    let summary: serde_json::Value =
        serde_json::from_slice(&apply_out.stdout).expect("parse summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));
    assert!(src.exists(), "source unchanged");
    assert!(target.exists(), "destination remains");
    assert!(target.read_dir().unwrap().count() == 0, "destination empty");
}
