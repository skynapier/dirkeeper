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

fn read_plan(task: &Path) -> serde_json::Value {
    let s = fs::read_to_string(task.join("plan.json")).expect("read plan.json");
    serde_json::from_str(&s).expect("parse plan.json")
}

fn write_journal(task: &Path, id: &str, stages: &[&str]) {
    let mut buf = String::new();
    for st in stages {
        let ev = serde_json::json!({ "task_id": id, "index": 0, "stage": st, "reason": null });
        buf.push_str(&ev.to_string());
        buf.push('\n');
    }
    fs::write(task.join("journal.jsonl"), buf).expect("write journal");
}

fn write_evidence(task: &Path, id: &str, src: &str, dst: &str, manifest: &serde_json::Value) {
    let ev = serde_json::json!({
        "task_id": id,
        "index": 0,
        "source": src,
        "destination": dst,
        "manifest": manifest,
    });
    fs::write(task.join("evidence").join("000000.json"), ev.to_string()).expect("write evidence");
}

fn stage_path(task: &Path) -> std::path::PathBuf {
    let plan = read_plan(task);
    let id = plan["id"].as_str().expect("id").to_string();
    let dst = plan["moves"][0]["destination"].as_str().expect("dst");
    let parent = Path::new(dst).parent().expect("dst parent");
    parent.join(format!(".dirplace-{}-000000.staging", id))
}

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let src = root.join("src");
    let dst = root.join("dst");
    let task = root.join("task");

    fs::create_dir_all(&src).expect("src");
    fs::create_dir_all(&dst).expect("dst");
    fs::write(src.join("a"), "A").expect("src/a");
    fs::write(src.join("b"), "B").expect("src/b");
    fs::write(dst.join("keep"), "K").expect("dst/keep");

    let input = root.join("input.json");
    let input_json = serde_json::json!([
        { "source": src.to_string_lossy(), "destination": dst.to_string_lossy() }
    ]);
    fs::write(&input, input_json.to_string()).expect("input");

    let out = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task.to_str().unwrap(),
        "--resolve-existing",
    ]);
    assert!(
        out.status.success(),
        "plan failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    (tmp, src, dst, task)
}

#[test]
fn partial_publication_resumes_to_union() {
    let (_tmp, src, dst, task) = setup();
    let plan = read_plan(&task);
    let id = plan["id"].as_str().expect("id").to_string();
    let src_s = plan["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst_s = plan["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest = plan["moves"][0]["reconciliation"]["source_manifest"].clone();

    // a already published, b still in source, partial staging holding b.
    fs::write(dst.join("a"), "A").expect("dst/a");
    fs::write(src.join("b"), "B").expect("src/b");
    let stage = stage_path(&task);
    fs::create_dir_all(&stage).expect("stage");
    fs::write(stage.join("b"), "B").expect("stage/b");

    write_evidence(&task, &id, &src_s, &dst_s, &manifest);
    write_journal(&task, &id, &["merging", "error"]);

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("summary");
    assert_eq!(summary["completed"].as_u64(), Some(1));
    assert_eq!(summary["conflicts"].as_u64(), Some(0));
    assert_eq!(summary["errors"].as_u64(), Some(0));

    assert_eq!(fs::read_to_string(dst.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(dst.join("b")).unwrap(), "B");
    assert_eq!(fs::read_to_string(dst.join("keep")).unwrap(), "K");
    assert!(!src.exists(), "source should be deleted");
    assert!(!stage.exists(), "staging should be deleted");

    // Repeated resume must still succeed (idempotent, completed).
    let out2 = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        out2.status.success(),
        "repeat resume failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let summary2: serde_json::Value = serde_json::from_slice(&out2.stdout).expect("summary2");
    assert_eq!(summary2["completed"].as_u64(), Some(1));
    assert_eq!(summary2["conflicts"].as_u64(), Some(0));
    assert_eq!(summary2["errors"].as_u64(), Some(0));
    assert!(!src.exists(), "source still deleted");
    assert!(!stage.exists(), "staging still deleted");
}

#[test]
fn partial_deletion_resumes_to_completed() {
    let (_tmp, src, dst, task) = setup();
    let plan = read_plan(&task);
    let id = plan["id"].as_str().expect("id").to_string();
    let src_s = plan["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst_s = plan["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest = plan["moves"][0]["reconciliation"]["source_manifest"].clone();

    // dst already holds the full union; source only has b left.
    fs::write(dst.join("a"), "A").expect("dst/a");
    fs::write(dst.join("b"), "B").expect("dst/b");
    fs::write(dst.join("keep"), "K").expect("dst/keep");
    fs::write(src.join("b"), "B").expect("src/b");
    fs::remove_file(src.join("a")).expect("remove src/a");

    write_evidence(&task, &id, &src_s, &dst_s, &manifest);
    write_journal(&task, &id, &["deleting", "error"]);

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "resume deleting failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("summary");
    assert_eq!(summary["completed"].as_u64(), Some(1));
    assert_eq!(summary["conflicts"].as_u64(), Some(0));
    assert_eq!(summary["errors"].as_u64(), Some(0));
    assert!(!src.exists(), "remaining source should be removed");
    assert_eq!(fs::read_to_string(dst.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(dst.join("b")).unwrap(), "B");
    assert_eq!(fs::read_to_string(dst.join("keep")).unwrap(), "K");

    // Journal must end with a completed event.
    let journal = fs::read_to_string(task.join("journal.jsonl")).expect("read journal");
    let lines: Vec<&str> = journal.lines().collect();
    let last: serde_json::Value =
        serde_json::from_str(lines.last().expect("last line")).expect("last");
    assert_eq!(last["stage"].as_str(), Some("completed"));
}

#[test]
fn source_absent_deleting_resumes_to_completed() {
    let (_tmp, src, dst, task) = setup();
    let plan = read_plan(&task);
    let id = plan["id"].as_str().expect("id").to_string();
    let src_s = plan["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst_s = plan["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest = plan["moves"][0]["reconciliation"]["source_manifest"].clone();

    // Source wholly absent; dst holds the full union.
    fs::remove_dir_all(&src).expect("remove src");
    fs::write(dst.join("a"), "A").expect("dst/a");
    fs::write(dst.join("b"), "B").expect("dst/b");
    fs::write(dst.join("keep"), "K").expect("dst/keep");

    write_evidence(&task, &id, &src_s, &dst_s, &manifest);
    write_journal(&task, &id, &["deleting", "error"]);

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "resume absent-source failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("summary");
    assert_eq!(summary["completed"].as_u64(), Some(1));
    assert_eq!(summary["conflicts"].as_u64(), Some(0));
    assert_eq!(summary["errors"].as_u64(), Some(0));
    assert!(!src.exists(), "source absent");
    assert_eq!(fs::read_to_string(dst.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(dst.join("b")).unwrap(), "B");
    assert_eq!(fs::read_to_string(dst.join("keep")).unwrap(), "K");

    let journal = fs::read_to_string(task.join("journal.jsonl")).expect("read journal");
    let lines: Vec<&str> = journal.lines().collect();
    let last: serde_json::Value =
        serde_json::from_str(lines.last().expect("last line")).expect("last");
    assert_eq!(last["stage"].as_str(), Some("completed"));
}

#[test]
fn unexpected_staging_without_merging_refused() {
    let (_tmp, src, dst, task) = setup();
    let plan = read_plan(&task);
    let id = plan["id"].as_str().expect("id").to_string();
    let src_s = plan["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst_s = plan["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest = plan["moves"][0]["reconciliation"]["source_manifest"].clone();

    // Case A: staging present but no merging ownership (journal empty).
    let stage = stage_path(&task);
    fs::create_dir_all(&stage).expect("stage");
    fs::write(stage.join("b"), "B").expect("stage/b");
    write_evidence(&task, &id, &src_s, &dst_s, &manifest);
    write_journal(&task, &id, &[]);

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!out.status.success(), "should refuse");
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));
    assert!(stage.exists(), "staging must be preserved");
    assert!(src.exists(), "source must be preserved");
    assert_eq!(fs::read_to_string(dst.join("keep")).unwrap(), "K");

    // Case B: owned (merging) staging that contains a symlink must refuse,
    // preserving the symlink and its outside target.
    let (_tmp2, src2, dst2, task2) = setup();
    let plan2 = read_plan(&task2);
    let id2 = plan2["id"].as_str().expect("id").to_string();
    let src2_s = plan2["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst2_s = plan2["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest2 = plan2["moves"][0]["reconciliation"]["source_manifest"].clone();

    let outside = _tmp2.path().join("outside_target");
    fs::write(&outside, "OUTSIDE").expect("outside target");

    let stage2 = stage_path(&task2);
    fs::create_dir_all(&stage2).expect("stage2");
    fs::write(stage2.join("b"), "B").expect("stage2/b");
    std::os::unix::fs::symlink(&outside, stage2.join("link")).expect("symlink");

    write_evidence(&task2, &id2, &src2_s, &dst2_s, &manifest2);
    write_journal(&task2, &id2, &["merging"]);

    let out2 = run(&["resume", "--task-dir", task2.to_str().unwrap()]);
    assert!(!out2.status.success(), "should refuse symlink staging");
    let summary2: serde_json::Value = serde_json::from_slice(&out2.stdout).expect("summary2");
    assert_eq!(summary2["errors"].as_u64(), Some(1));
    assert_eq!(summary2["conflicts"].as_u64(), Some(0));
    assert!(stage2.exists(), "staging2 must be preserved");
    assert!(stage2.join("link").exists(), "symlink must be preserved");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "OUTSIDE");
    assert!(src2.exists(), "source2 must be preserved");
    assert_eq!(fs::read_to_string(dst2.join("keep")).unwrap(), "K");
}

#[test]
fn changed_baseline_refused_preserves_all() {
    let (_tmp, src, dst, task) = setup();
    let plan = read_plan(&task);
    let id = plan["id"].as_str().expect("id").to_string();
    let src_s = plan["moves"][0]["source"]
        .as_str()
        .expect("src")
        .to_string();
    let dst_s = plan["moves"][0]["destination"]
        .as_str()
        .expect("dst")
        .to_string();
    let manifest = plan["moves"][0]["reconciliation"]["source_manifest"].clone();

    // dst/keep changed bytes after planning; source intact; partial staging.
    fs::write(dst.join("keep"), "CHANGED").expect("dst/keep changed");
    fs::write(src.join("a"), "A").expect("src/a");
    fs::write(src.join("b"), "B").expect("src/b");
    let stage = stage_path(&task);
    fs::create_dir_all(&stage).expect("stage");
    fs::write(stage.join("b"), "B").expect("stage/b");

    write_evidence(&task, &id, &src_s, &dst_s, &manifest);
    write_journal(&task, &id, &["merging", "error"]);

    let out = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert!(!out.status.success(), "should refuse changed baseline");
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout).expect("summary");
    assert_eq!(summary["conflicts"].as_u64(), Some(1));

    assert!(src.exists(), "source must be preserved");
    assert_eq!(fs::read_to_string(src.join("a")).unwrap(), "A");
    assert_eq!(fs::read_to_string(src.join("b")).unwrap(), "B");
    assert!(stage.exists(), "staging must be preserved");
    assert_eq!(fs::read_to_string(dst.join("keep")).unwrap(), "CHANGED");
}
