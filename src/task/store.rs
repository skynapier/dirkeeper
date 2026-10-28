use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::model::PlanMove;
use crate::planning::paths_overlap;
use crate::task::model::{Event, Evidence, Manifest, ManifestEntry, Stage, TaskPlan, TaskSummary};

pub(crate) struct TaskStore {
    pub(crate) dir: PathBuf,
    pub(crate) plan: TaskPlan,
    pub(crate) events: Vec<Event>,
    _lock: File,
    poisoned: bool,
}

pub(crate) fn plan_id(
    version: u32,
    roots: &[PathBuf],
    moves: &[PlanMove],
) -> Result<String, String> {
    let tuple = (version, roots, moves);
    let json = serde_json::to_string(&tuple).map_err(|e| e.to_string())?;
    let hash = blake3::hash(json.as_bytes());
    Ok(hash.to_hex().to_string())
}

// 1. Reject any lexical dot / dotdot component.
fn reject_dotdot(p: &Path) -> Result<(), String> {
    let s = p.to_string_lossy();
    for seg in s.split('/') {
        if seg == "." || seg == ".." {
            return Err(format!("path contains . or ..: {}", p.display()));
        }
    }
    Ok(())
}

// 2. Path must be absolute and contain no dot / dotdot components.
fn require_absolute_lexical(p: &Path) -> Result<(), String> {
    if !p.is_absolute() {
        return Err(format!("path not absolute: {}", p.display()));
    }
    reject_dotdot(p)
}

// 3. Reject a path that is the root or a prefix of the task directory.
fn reject_root_or_prefix(task: &Path, p: &Path) -> Result<(), String> {
    if p.as_os_str().is_empty() || p == Path::new("/") {
        return Err(format!("path is root: {}", p.display()));
    }
    if p == task || task.starts_with(p) {
        return Err(format!("path overlaps task dir: {}", p.display()));
    }
    Ok(())
}

// 4. Validate plan version, id hash, move statuses, and all source/dest/cleanup
//    paths (absolute, lexical, no root, no overlap with task dir).
fn validate_plan(plan: &TaskPlan, task: &Path) -> Result<(), String> {
    if plan.version != 2 {
        return Err("Unsupported plan version".to_string());
    }
    let computed_id = plan_id(plan.version, &plan.cleanup_roots, &plan.moves)?;
    if computed_id != plan.id {
        return Err("Plan hash mismatch".to_string());
    }
    for root in &plan.cleanup_roots {
        require_absolute_lexical(root)?;
        reject_root_or_prefix(task, root)?;
        if paths_overlap(task, root) {
            return Err("Cleanup root overlaps task directory".to_string());
        }
    }
    for m in &plan.moves {
        let src = Path::new(&m.source);
        let dst = Path::new(&m.destination);
        require_absolute_lexical(src)?;
        require_absolute_lexical(dst)?;
        reject_root_or_prefix(task, src)?;
        reject_root_or_prefix(task, dst)?;
        if paths_overlap(task, src) || paths_overlap(task, dst) {
            return Err("Move overlaps task directory".to_string());
        }
        crate::task::reconcile::validate_resolution(m)?;
        if let Some(rec) = &m.reconciliation {
            let od = Path::new(&rec.original_destination);
            require_absolute_lexical(od)?;
            reject_root_or_prefix(task, od)?;
            if paths_overlap(task, od) {
                return Err(
                    "Reconciliation original destination overlaps task directory".to_string(),
                );
            }
        }
    }
    Ok(())
}

// 5. Open a file with O_NOFOLLOW and confirm it is a regular (non-symlink) file.
fn open_regular(path: &Path) -> Result<File, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("Cannot stat {}: {}", path.display(), e))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("Cannot open {}: {}", path.display(), e))
}

// 6. Confirm a directory exists, is not a symlink, and is a directory.
fn check_dir(path: &Path) -> Result<(), String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("Cannot stat {}: {}", path.display(), e))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    Ok(())
}

// 7. A manifest value is either the directory marker or a file entry with a 64-char lowercase hex hash.
fn valid_manifest_value(v: &ManifestEntry) -> bool {
    match v {
        ManifestEntry::Dir => true,
        ManifestEntry::File { hash, .. } => {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| (b >= b'0' && b <= b'9') || (b >= b'a' && b <= b'f'))
        }
    }
}

// 8. Validate every manifest key (relative, normal components only) and value.
fn validate_manifest(m: &Manifest) -> Result<(), String> {
    for (k, v) in m {
        if k.is_empty() {
            return Err("Empty manifest key".to_string());
        }
        for seg in k.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return Err(format!("Invalid manifest key segment: {}", k));
            }
        }
        if !valid_manifest_value(v) {
            return Err(format!("Invalid manifest value for: {}", k));
        }
    }
    Ok(())
}

// 9. Validate a full journal line: parse, check task_id, index bounds, known stage.
fn parse_event_line(line: &str, plan: &TaskPlan) -> Result<Event, String> {
    let ev: Event =
        serde_json::from_str(line).map_err(|e| format!("Invalid journal record: {}", e))?;
    if ev.task_id != plan.id {
        return Err("Journal record task_id mismatch".to_string());
    }
    if ev.index >= plan.moves.len() {
        return Err("Journal record index out of bounds".to_string());
    }
    Ok(ev)
}

// 10. Read the journal, validate every terminated line, and truncate only the
//     trailing bytes after the final newline (crash-safe, no full rewrite).
fn read_journal(path: &Path, plan: &TaskPlan) -> Result<Vec<Event>, String> {
    let mut file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read journal: {}", e))?;
    let byte_count = bytes.len();
    let mut offset = 0usize;
    for (i, b) in bytes.iter().rev().enumerate() {
        if *b == b'\n' {
            offset = byte_count - i;
            break;
        }
    }
    let prefix = &bytes[..offset];
    let s = String::from_utf8(prefix.to_vec())
        .map_err(|e| format!("Journal not valid UTF-8: {}", e))?;
    let mut events = Vec::new();
    for line in s.split_terminator('\n') {
        if line.is_empty() {
            return Err("Empty line in journal".to_string());
        }
        events.push(parse_event_line(line, plan)?);
    }
    if offset < byte_count {
        let f = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| format!("Cannot open journal for truncate: {}", e))?;
        f.set_len(offset as u64)
            .map_err(|e| format!("Cannot truncate journal: {}", e))?;
        f.sync_all()
            .map_err(|e| format!("Cannot sync journal: {}", e))?;
    }
    Ok(events)
}

impl TaskStore {
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        // 1. Reject lexical dot/dotdot before any open.
        reject_dotdot(dir)?;
        // 2. Canonicalize and require the input to be already canonical.
        let canon_dir = dir
            .canonicalize()
            .map_err(|e| format!("Cannot canonicalize dir: {}", e))?;
        if canon_dir != dir {
            return Err("Task dir is not canonical".to_string());
        }
        check_dir(&canon_dir)?;
        // 3. Validate the plan (version, hash, statuses, paths, overlaps) BEFORE any open.
        let plan_path = canon_dir.join("plan.json");
        let plan_file = open_regular(&plan_path)?;
        let plan: TaskPlan =
            serde_json::from_reader(plan_file).map_err(|e| format!("Invalid plan.json: {}", e))?;
        validate_plan(&plan, &canon_dir)?;
        // 4. Validate the evidence directory BEFORE any open.
        check_dir(&canon_dir.join("evidence"))?;
        // 5. Acquire a non-blocking exclusive lock (RAII via _lock).
        let lock_path = canon_dir.join("lock");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|e| format!("Cannot open lock file: {}", e))?;
        let lock_meta = fs::symlink_metadata(&lock_path)
            .map_err(|e| format!("Cannot stat lock file: {}", e))?;
        if lock_meta.file_type().is_symlink() || !lock_meta.is_file() {
            return Err("Lock file is not a regular file".to_string());
        }
        let res = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if res != 0 {
            return Err("Failed to acquire lock".to_string());
        }
        // 6. Re-validate files while locked.
        check_dir(&canon_dir.join("evidence"))?;
        let events = read_journal(&canon_dir.join("journal.jsonl"), &plan)?;
        Ok(Self {
            dir: canon_dir,
            plan,
            events,
            _lock: lock_file,
            poisoned: false,
        })
    }

    pub(crate) fn create(dir: &Path, plan: TaskPlan) -> Result<Self, String> {
        // 1. Reject lexical dot/dotdot before any open.
        reject_dotdot(dir)?;
        // 2. Validate the plan against the intended task dir.
        let parent = dir.parent().ok_or("Task dir has no parent")?;
        let file_name = dir.file_name().ok_or("Invalid task dir name")?;
        let canon_parent = parent
            .canonicalize()
            .map_err(|e| format!("Cannot canonicalize parent: {}", e))?;
        let task_dir = canon_parent.join(file_name);
        validate_plan(&plan, &task_dir)?;
        // 3. Create the task dir only if the canonical parent equals the supplied parent.
        if canon_parent != parent {
            return Err("Task dir parent is not canonical".to_string());
        }
        if task_dir.exists() {
            return Err("Task directory already exists".to_string());
        }
        fs::create_dir(&task_dir).map_err(|e| format!("Cannot create task dir: {}", e))?;
        let evidence_dir = task_dir.join("evidence");
        fs::create_dir(&evidence_dir).map_err(|e| format!("Cannot create evidence dir: {}", e))?;
        // 4. Write plan.json (create_new, synced).
        let plan_path = task_dir.join("plan.json");
        let plan_json = serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?;
        let mut plan_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&plan_path)
            .map_err(|e| format!("Cannot create plan.json: {}", e))?;
        plan_file
            .write_all(plan_json.as_bytes())
            .map_err(|e| format!("Cannot write plan.json: {}", e))?;
        plan_file
            .sync_all()
            .map_err(|e| format!("Cannot sync plan.json: {}", e))?;
        // 5. Write empty journal.jsonl (create_new, synced).
        let journal_path = task_dir.join("journal.jsonl");
        let journal_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&journal_path)
            .map_err(|e| format!("Cannot create journal.jsonl: {}", e))?;
        journal_file
            .sync_all()
            .map_err(|e| format!("Cannot sync journal.jsonl: {}", e))?;
        // 6. Fsync the new evidence dir, task dir, and task parent.
        for d in [&evidence_dir, &task_dir, &canon_parent] {
            let df = File::open(d).map_err(|e| format!("Cannot open dir: {}", e))?;
            df.sync_all()
                .map_err(|e| format!("Cannot sync dir: {}", e))?;
        }
        // 7. Open the store.
        Self::open(&task_dir)
    }

    pub(crate) fn event(
        &mut self,
        index: usize,
        stage: Stage,
        reason: Option<String>,
    ) -> Result<(), String> {
        // 1. Reject once poisoned (a prior append may have torn the journal).
        if self.poisoned {
            return Err("Store poisoned after journal append error".to_string());
        }
        // 2. Validate index and stage.
        if index >= self.plan.moves.len() {
            return Err("Index out of bounds".to_string());
        }
        // 3. Serialize the whole record plus its newline into one buffer.
        let event = Event {
            task_id: self.plan.id.clone(),
            index,
            stage,
            reason,
        };
        let mut buf = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        buf.push('\n');
        // 4. Append with a single write_all, then sync; only update memory on success.
        let journal_path = self.dir.join("journal.jsonl");
        let mut file = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&journal_path)
            .map_err(|e| format!("Cannot open journal: {}", e))?;
        if let Err(e) = file.write_all(buf.as_bytes()).and_then(|_| file.sync_all()) {
            self.poisoned = true;
            return Err(format!("Cannot append journal: {}", e));
        }
        self.events.push(event);
        Ok(())
    }

    pub(crate) fn evidence(&self, index: usize) -> Result<Option<Evidence>, String> {
        // 1. Validate index and re-check the evidence dir.
        if index >= self.plan.moves.len() {
            return Err("Index out of bounds".to_string());
        }
        check_dir(&self.dir.join("evidence"))?;
        // 2. Resolve the final path; missing is Ok(None).
        let path = self.dir.join("evidence").join(format!("{:06}.json", index));
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("Cannot stat evidence: {}", e)),
        };
        if meta.file_type().is_symlink() {
            return Err("Evidence file is a symlink".to_string());
        }
        if !meta.is_file() {
            return Err("Evidence file is not a regular file".to_string());
        }
        // 3. Read and parse.
        let file = open_regular(&path)?;
        let evidence: Evidence =
            serde_json::from_reader(file).map_err(|e| format!("Invalid evidence: {}", e))?;
        // 4. Validate task_id, index, source, destination, and manifest.
        if evidence.task_id != self.plan.id {
            return Err("Evidence task_id mismatch".to_string());
        }
        if evidence.index != index {
            return Err("Evidence index mismatch".to_string());
        }
        let m = &self.plan.moves[index];
        if evidence.source != m.source {
            return Err("Evidence source mismatch".to_string());
        }
        if evidence.destination != m.destination {
            return Err("Evidence destination mismatch".to_string());
        }
        validate_manifest(&evidence.manifest)?;
        if let Some(rec) = &m.reconciliation {
            if evidence.manifest != rec.source_manifest {
                return Err(
                    "Evidence manifest does not match reconciliation source manifest".to_string(),
                );
            }
        }
        Ok(Some(evidence))
    }

    pub(crate) fn save_evidence(&self, evidence: &Evidence) -> Result<(), String> {
        // 1. Validate index, task_id, source, destination, and manifest.
        if evidence.index >= self.plan.moves.len() {
            return Err("Index out of bounds".to_string());
        }
        if evidence.task_id != self.plan.id {
            return Err("Evidence task_id mismatch".to_string());
        }
        let m = &self.plan.moves[evidence.index];
        if evidence.source != m.source {
            return Err("Evidence source mismatch".to_string());
        }
        if evidence.destination != m.destination {
            return Err("Evidence destination mismatch".to_string());
        }
        validate_manifest(&evidence.manifest)?;
        if let Some(rec) = &m.reconciliation {
            if evidence.manifest != rec.source_manifest {
                return Err(
                    "Evidence manifest does not match reconciliation source manifest".to_string(),
                );
            }
        }
        // 2. Re-check the evidence dir.
        let ev_dir = self.dir.join("evidence");
        check_dir(&ev_dir)?;
        // 3. The final file must be absent; a stale reserved temp may be removed only if
        //    it is a regular non-symlink file under the exact evidence dir.
        let final_path = ev_dir.join(format!("{:06}.json", evidence.index));
        if fs::symlink_metadata(&final_path).is_ok() {
            return Err("Evidence file already exists".to_string());
        }
        let tmp_path = ev_dir.join(format!("{:06}.json.tmp", evidence.index));
        if let Ok(tm) = fs::symlink_metadata(&tmp_path) {
            if tm.file_type().is_symlink() || !tm.is_file() {
                return Err("Stale evidence temp is not a regular file".to_string());
            }
            fs::remove_file(&tmp_path).map_err(|e| format!("Cannot remove stale temp: {}", e))?;
        }
        // 4. Serialize.
        let json = serde_json::to_string_pretty(evidence).map_err(|e| e.to_string())?;
        // 5. Write the reserved temp (create_new), fsync, then atomically publish.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp_path)
            .map_err(|e| format!("Cannot create evidence temp: {}", e))?;
        file.write_all(json.as_bytes())
            .map_err(|e| format!("Cannot write evidence temp: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Cannot sync evidence temp: {}", e))?;
        crate::filesystem::atomic_rename(&tmp_path, &final_path)
            .map_err(|e| format!("Cannot publish evidence: {}", e))?;
        // 6. Fsync the evidence dir.
        let df = File::open(&ev_dir).map_err(|e| format!("Cannot open evidence dir: {}", e))?;
        df.sync_all()
            .map_err(|e| format!("Cannot sync evidence dir: {}", e))?;
        Ok(())
    }

    pub(crate) fn write_summary(&self, summary: &TaskSummary) -> Result<(), String> {
        // 1. Paths.
        let summary_path = self.dir.join("summary.json");
        let temp_path = self.dir.join("summary.json.tmp");
        // 2. Remove a stale regular temp before create_new; reject symlink/directory.
        if let Ok(tm) = fs::symlink_metadata(&temp_path) {
            if tm.file_type().is_symlink() || !tm.is_file() {
                return Err("Summary temp is not a regular file".to_string());
            }
            fs::remove_file(&temp_path)
                .map_err(|e| format!("Cannot remove stale summary temp: {}", e))?;
        }
        // 3. The final summary must be absent or a regular non-symlink file.
        if let Ok(sm) = fs::symlink_metadata(&summary_path) {
            if sm.file_type().is_symlink() || !sm.is_file() {
                return Err("Summary final is not a regular file".to_string());
            }
        }
        // 4. Serialize.
        let json = serde_json::to_string_pretty(summary).map_err(|e| e.to_string())?;
        // 5. Write the temp (create_new), fsync, then atomically replace.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp_path)
            .map_err(|e| format!("Cannot create temp summary: {}", e))?;
        file.write_all(json.as_bytes())
            .map_err(|e| format!("Cannot write temp summary: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Cannot sync temp summary: {}", e))?;
        fs::rename(&temp_path, &summary_path)
            .map_err(|e| format!("Cannot rename summary: {}", e))?;
        // 6. Fsync the task dir.
        let df = File::open(&self.dir).map_err(|e| format!("Cannot open dir: {}", e))?;
        df.sync_all()
            .map_err(|e| format!("Cannot sync dir: {}", e))?;
        Ok(())
    }

    pub(crate) fn stage(&self, index: usize) -> Option<Stage> {
        // 1. Most recent stage for this index, excluding terminal stages.
        self.events
            .iter()
            .rev()
            .find(|e| {
                e.index == index
                    && !e.stage.is_failure()
                    && e.stage != Stage::Cleaned
                    && e.stage != Stage::ParentCreated
            })
            .map(|e| e.stage)
    }

    pub(crate) fn staging(&self, index: usize) -> PathBuf {
        // 1. Deterministic staging path under the destination's parent.
        let dst = PathBuf::from(&self.plan.moves[index].destination);
        let parent = dst.parent().unwrap_or(Path::new("/"));
        parent.join(format!(".dirplace-{}-{:06}.staging", self.plan.id, index))
    }
}
