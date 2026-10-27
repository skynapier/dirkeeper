// src/task_engine.rs
//
// Orchestration layer for dirkeeper tasks.
//
// Public(crate) API:
//   prepare  – read a JSON spec file, build a v2 TaskPlan, create the task dir.
//   execute  – run ready/conflict moves up to an optional cap.
//   status   – read-only summary of the current task state.
//   cleanup  – invoke cleanup_owned for every ready move, then write summary.
//
// No filesystem primitives are re-implemented here; all I/O goes through
// task_store, task_move, task_validate, and planning.

use std::path::{Path, PathBuf};

use crate::model::{MoveSpec, MoveStatus, PlanMove};
use crate::planning::{generate_plan, paths_overlap, validate_cleanup_root};
use crate::task::item::{MoveError, cleanup_owned, is_completed, perform};
use crate::task::model::{Stage, TaskPlan, TaskSummary};
use crate::task::store::{TaskStore, plan_id};
use crate::task::validate::validate_task;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a TaskSummary by inspecting the store's journal events and the
/// current plan.  Each move is classified exactly once.
///
/// Rules (mirrored from the spec):
///   * A move whose last journal event is "conflict" or "error" is counted
///     in the matching bucket.
///   * A move whose last journal event is "completed" is counted as completed
///     ONLY if `task_move::is_completed` returns true; otherwise it is a
///     conflict.
///   * Every other move is pending.
///   * Total = completed + pending + conflicts + errors.
fn build_summary(store: &TaskStore) -> Result<TaskSummary, String> {
    let n = store.plan.moves.len();
    let mut completed: usize = 0;
    let mut pending: usize = 0;
    let mut conflicts: usize = 0;
    let mut errors: usize = 0;

    for i in 0..n {
        // Determine the last journal event for this index.
        let last_event = store.events.iter().rev().find(|ev| ev.index == i);

        match last_event {
            Some(ev) if ev.stage == Stage::Conflict => {
                conflicts += 1;
            }
            Some(ev) if ev.stage == Stage::Error => {
                errors += 1;
            }
            Some(ev) if ev.stage == Stage::Completed => {
                // Must verify via is_completed; if it fails, treat as conflict.
                match is_completed(store, i) {
                    Ok(true) => completed += 1,
                    Ok(false) => conflicts += 1,
                    Err(_) => conflicts += 1,
                }
            }
            _ => {
                // No event, or a non-terminal stage (copying, verified,
                // published, deleting, cleaned) → pending.
                if store.plan.moves[i].status == MoveStatus::Conflict {
                    conflicts += 1;
                } else {
                    pending += 1;
                }
            }
        }
    }

    Ok(TaskSummary {
        total: n,
        completed,
        pending,
        conflicts,
        errors,
    })
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

/// 1. Read the JSON spec file (Vec<MoveSpec>).
/// 2. Reject a limit of 0.
/// 3. Optionally truncate the spec list to `limit` entries.
/// 4. Validate every cleanup root via `planning::validate_cleanup_root`.
/// 5. Reject any cleanup root that overlaps any destination.
/// 6. Build a v2 TaskPlan with a deterministic id from `task_store::plan_id`.
/// 7. Create the task directory via `TaskStore::create` (returns a held lock).
/// 8. Run `validate_task` on the freshly created store.
/// 9. Write the initial summary and return it.
///
/// No source or destination is mutated in this function.
pub(crate) fn prepare(
    input: &Path,
    task_dir: &Path,
    cleanup_roots: &[PathBuf],
    limit: Option<usize>,
    resolve_existing: bool,
) -> Result<TaskSummary, String> {
    // 1. Read and parse the JSON spec file (or stdin if input is "-").
    let specs: Vec<MoveSpec> = if input == Path::new("-") {
        serde_json::from_reader(std::io::stdin().lock())
            .map_err(|e| format!("Invalid spec JSON: {}", e))?
    } else {
        let file =
            std::fs::File::open(input).map_err(|e| format!("Cannot open input file: {}", e))?;
        serde_json::from_reader(file).map_err(|e| format!("Invalid spec JSON: {}", e))?
    };

    // 2. Reject a limit of 0.
    if limit == Some(0) {
        return Err("Limit must not be zero".to_string());
    }

    // 3. Optionally truncate the spec list.
    let specs: Vec<MoveSpec> = match limit {
        Some(n) => specs.into_iter().take(n).collect(),
        None => specs,
    };

    // 4. Validate every cleanup root.
    for root in cleanup_roots {
        validate_cleanup_root(root)
            .map_err(|e| format!("Invalid cleanup root {}: {}", root.display(), e))?;
    }

    // 5. Reject any cleanup root that overlaps any destination.
    for root in cleanup_roots {
        for spec in &specs {
            let dst = Path::new(&spec.destination);
            if paths_overlap(root, dst) {
                return Err(format!(
                    "Cleanup root {} overlaps destination {}",
                    root.display(),
                    dst.display()
                ));
            }
        }
    }

    // 6. Build a v2 TaskPlan.
    //    Use planning::generate_plan to classify each move as ready/conflict.
    let plan_v1 = generate_plan(&specs);
    let mut moves: Vec<PlanMove> = plan_v1.moves;

    // 6a. If resolve_existing is enabled, reconcile moves before plan creation.
    if resolve_existing {
        crate::task::reconcile::resolve_moves(&mut moves)?;
    }

    let id = plan_id(2, cleanup_roots, &moves)?;
    let plan = TaskPlan {
        version: 2,
        id,
        cleanup_roots: cleanup_roots.to_vec(),
        moves,
    };

    // 7. Create the task directory (returns a store with a held lock).
    let store = TaskStore::create(task_dir, plan)?;

    // 8. Validate the task (evidence dir, move paths, overlaps, cleanup roots).
    validate_task(&store)?;

    // 9. Write and return the initial summary.
    let summary = build_summary(&store)?;
    store.write_summary(&summary)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// 1. Open the task store (acquires the lock).
/// 2. Validate the task before any data mutation.
/// 3. Clone the moves list to avoid borrow conflicts.
/// 4. For each move:
///      * "conflict" status → count as conflict, do not attempt.
///      * "ready" status → attempt via `task_move::perform`, up to `max_items`.
///      * A "completed" phase is always performed (to verify/cleanup) even
///        after the cap is reached.
/// 5. Classify unattempted rows from the journal.
/// 6. Write and return the full summary.
pub(crate) fn execute(task_dir: &Path, max_items: Option<usize>) -> Result<TaskSummary, String> {
    // 0. Reject a zero cap before opening the task.
    if max_items == Some(0) {
        return Err("Max items must not be zero".to_string());
    }

    // 1. Open the store.
    let mut store = TaskStore::open(task_dir)?;

    // 2. Validate before any mutation.
    validate_task(&store)?;

    // 3. Clone moves so we can iterate without holding a borrow on `store`.
    let moves = store.plan.moves.clone();
    let n = moves.len();

    // 4. Process each move.
    let mut attempted: usize = 0;

    for i in 0..n {
        let status = moves[i].status;

        // Conflict rows are counted but never attempted.
        if status == MoveStatus::Conflict {
            continue;
        }

        // Determine whether this row is in a "completed" phase.
        // A completed phase must always be performed (verify + cleanup),
        // even after the cap is reached.
        let is_completed_phase = store.stage(i) == Some(Stage::Completed);

        // Enforce the cap for non-completed ready rows.
        if !is_completed_phase {
            if let Some(cap) = max_items {
                if attempted >= cap {
                    continue;
                }
            }
        }

        // Attempt the move.
        match perform(&mut store, i) {
            Ok(()) => {
                // perform succeeded; the journal already has the terminal event.
                // A completed phase does not consume the cap.
                if !is_completed_phase {
                    attempted += 1;
                }
            }
            Err(MoveError::Conflict(reason)) => {
                // Record the conflict in the journal.
                store
                    .event(i, Stage::Conflict, Some(reason))
                    .map_err(|e| format!("Journal append failed (conflict): {}", e))?;
                if !is_completed_phase {
                    attempted += 1;
                }
            }
            Err(MoveError::Io(reason)) => {
                // Record the error in the journal.
                store
                    .event(i, Stage::Error, Some(reason))
                    .map_err(|e| format!("Journal append failed (error): {}", e))?;
                if !is_completed_phase {
                    attempted += 1;
                }
            }
        }
    }

    // 5. Build the full summary from the journal.
    let summary = build_summary(&store)?;

    // 6. Write and return.
    store.write_summary(&summary)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// 1. Open the task store (acquires the lock).
/// 2. Validate the task.
/// 3. Build the summary (no source copy or delete).
/// 4. Write and return the summary.
pub(crate) fn status(task_dir: &Path) -> Result<TaskSummary, String> {
    // 1. Open the store.
    let store = TaskStore::open(task_dir)?;

    // 2. Validate.
    validate_task(&store)?;

    // 3. Build summary.
    let summary = build_summary(&store)?;

    // 4. Write and return.
    store.write_summary(&summary)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// cleanup
// ---------------------------------------------------------------------------

/// 1. Open the task store (acquires the lock).
/// 2. Validate the task.
/// 3. For every "ready" move, call `cleanup_owned`.
/// 4. Classify errors the same way as execute (journal event on failure).
/// 5. Build the full summary and write it.
pub(crate) fn cleanup(task_dir: &Path) -> Result<TaskSummary, String> {
    // 1. Open the store.
    let mut store = TaskStore::open(task_dir)?;

    // 2. Validate.
    validate_task(&store)?;

    // 3. Clone moves to avoid borrow issues.
    let moves = store.plan.moves.clone();
    let n = moves.len();

    // 4. Process each ready move.
    for i in 0..n {
        if moves[i].status != MoveStatus::Ready {
            continue;
        }

        match cleanup_owned(&mut store, i) {
            Ok(()) => {
                // Success; journal already updated by cleanup_owned.
            }
            Err(MoveError::Conflict(reason)) => {
                store
                    .event(i, Stage::Conflict, Some(reason))
                    .map_err(|e| format!("Journal append failed (cleanup conflict): {}", e))?;
            }
            Err(MoveError::Io(reason)) => {
                store
                    .event(i, Stage::Error, Some(reason))
                    .map_err(|e| format!("Journal append failed (cleanup error): {}", e))?;
            }
        }
    }

    // 5. Build and write the full summary.
    let summary = build_summary(&store)?;
    store.write_summary(&summary)?;
    Ok(summary)
}
