use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

#[test]
fn stdin_valid_json_plans_successfully() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("source");
    let dest = root.join("dest");
    let task = root.join("task");

    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&dest).unwrap();
    assert!(!task.exists());

    let sb = source.join("book0");
    fs::create_dir_all(&sb).unwrap();
    fs::write(sb.join("smallfile"), "data0").unwrap();
    let db = dest.join("book0");

    let moves =
        serde_json::json!([{"source": sb.to_str().unwrap(), "destination": db.to_str().unwrap()}]);
    let input_json = serde_json::to_string(&moves).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dirkeeper"));
    cmd.arg("plan")
        .arg("--input")
        .arg("-")
        .arg("--task-dir")
        .arg(task.to_str().unwrap());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input_json.as_bytes()).unwrap();
    drop(stdin);

    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r.get("total").and_then(|x| x.as_u64()).unwrap(), 1);
    assert_eq!(r.get("pending").and_then(|x| x.as_u64()).unwrap(), 1);

    assert!(task.exists());
    assert!(sb.join("smallfile").exists());
    assert!(!db.exists());
}

#[test]
fn stdin_invalid_json_fails() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("source");
    let dest = root.join("dest");
    let task = root.join("task");

    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&dest).unwrap();
    assert!(!task.exists());

    let sb = source.join("book0");
    fs::create_dir_all(&sb).unwrap();
    fs::write(sb.join("smallfile"), "data0").unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dirkeeper"));
    cmd.arg("plan")
        .arg("--input")
        .arg("-")
        .arg("--task-dir")
        .arg(task.to_str().unwrap());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"invalid json").unwrap();
    drop(stdin);

    let out = child.wait_with_output().unwrap();
    assert_ne!(out.status.code(), Some(0));

    assert!(!task.exists());
    assert!(sb.join("smallfile").exists());
}
