use serde_json::Value;
use std::fs;

use std::process::Command;
use tempfile::TempDir;

fn run(args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_dirkeeper"))
        .args(args)
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn get(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap()
}

#[test]
fn cli_integration_3books() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("source");
    let dest = root.join("dest");
    let task = root.join("task");
    let input = root.join("moves.json");

    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&dest).unwrap();
    assert!(!task.exists());

    let mut moves = Vec::new();
    for i in 0..3 {
        let sb = source.join(format!("book{}", i));
        fs::create_dir_all(&sb).unwrap();
        fs::write(sb.join("smallfile"), format!("data{}", i)).unwrap();
        fs::write(sb.join("emptyfile"), "").unwrap();
        fs::create_dir_all(sb.join("emptydir")).unwrap();
        let db = dest.join(format!("book{}", i));
        moves.push(serde_json::json!({"source": sb.to_str().unwrap(), "destination": db.to_str().unwrap()}));
    }
    fs::write(&input, serde_json::to_string(&moves).unwrap()).unwrap();

    let r = run(&[
        "plan",
        "--input",
        input.to_str().unwrap(),
        "--task-dir",
        task.to_str().unwrap(),
        "--cleanup-root",
        source.to_str().unwrap(),
        "--limit",
        "3",
    ]);
    assert_eq!(get(&r, "total"), 3);
    assert_eq!(get(&r, "pending"), 3);

    let r = run(&[
        "apply",
        "--task-dir",
        task.to_str().unwrap(),
        "--max-items",
        "1",
    ]);
    assert_eq!(get(&r, "completed"), 1);
    assert_eq!(get(&r, "pending"), 2);

    let r = run(&[
        "resume",
        "--task-dir",
        task.to_str().unwrap(),
        "--max-items",
        "1",
    ]);
    assert_eq!(get(&r, "completed"), 2);
    assert_eq!(get(&r, "pending"), 1);

    let r = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert_eq!(get(&r, "completed"), 3);
    assert_eq!(get(&r, "pending"), 0);

    let r = run(&["resume", "--task-dir", task.to_str().unwrap()]);
    assert_eq!(get(&r, "completed"), 3);
    assert_eq!(get(&r, "pending"), 0);

    let r = run(&["cleanup", "--task-dir", task.to_str().unwrap()]);
    assert_eq!(get(&r, "completed"), 3);

    for i in 0..3 {
        let db = dest.join(format!("book{}", i));
        assert_eq!(
            fs::read(db.join("smallfile")).unwrap(),
            format!("data{}", i).into_bytes()
        );
        assert!(db.join("emptyfile").exists());
        assert!(db.join("emptydir").is_dir());
        let sb = source.join(format!("book{}", i));
        assert!(!sb.exists());
        let entries: Vec<_> = fs::read_dir(&db)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries.len(), 3);
        assert!(entries.contains(&"smallfile".to_string()));
        assert!(entries.contains(&"emptyfile".to_string()));
        assert!(entries.contains(&"emptydir".to_string()));
    }
    assert!(source.exists());
    let dest_entries: Vec<_> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(dest_entries.len(), 3);
    assert!(task.join("plan.json").exists());
    assert!(task.join("journal.jsonl").exists());
    assert!(task.join("evidence/000000.json").exists());
    assert!(task.join("summary.json").exists());
    assert!(task.join("lock").exists());
}
