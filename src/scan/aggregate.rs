use super::classify::classify_counts;
use super::types::{
    Counts, DirectoryRecord, Kind, ScanError, ScanIssue, ScanOptions, ScanSummary, Scope,
};
use super::walk::{WalkEvent, walk};
use std::io;
use std::path::{Path, PathBuf};

// Directory scan aggregation.
// 1. Validate options and build an Aggregator.
// 2. Walk the tree; the Aggregator handles each WalkEvent.
// 3. Flush remaining frames and return the summary.

/// Scans a directory tree, aggregating file counts per directory and emitting
/// [`DirectoryRecord`]s for directories that pass the configured filters.
///
/// # Steps
///
/// 1. Validate `min_ratio` is finite and in `[0.5, 1.0]`.
/// 2. Walk the tree, pushing a [`Frame`] per directory and incrementing
///    counts for files and child directories.
/// 3. Flush finished frames (deeper siblings first), aggregating subtree
///    counts into the parent.
/// 4. Classify each flushed frame and emit it if it passes the depth and
///    kind filters.
/// 5. Flush any remaining frames on the stack.
pub fn scan(
    root: &Path,
    options: &ScanOptions,
    emit: impl FnMut(&DirectoryRecord) -> io::Result<()>,
) -> Result<ScanSummary, ScanError> {
    // 1. Validate min_ratio
    let valid = (0.5..=1.0).contains(&options.min_ratio) && options.min_ratio.is_finite();
    if !valid {
        return Err(ScanError::InvalidOptions(format!(
            "min_ratio must be finite and in [0.5, 1.0], got {}",
            options.min_ratio
        )));
    }

    // 2. Build aggregator and walk
    let mut aggregator = Aggregator::new(options, emit);
    walk(root, options, |event| aggregator.handle(event))?;

    // 3. Flush remaining frames and return summary
    aggregator.flush_all()?;
    Ok(aggregator.summary)
}

/// A frame on the directory stack, tracking counts and issues for one directory.
struct Frame {
    path: PathBuf,
    relative_path: PathBuf,
    depth: usize,
    direct: Counts,
    subtree: Counts,
    child_dirs: u64,
    complete: bool,
    issues: Vec<ScanIssue>,
}

impl Frame {
    /// Creates a new frame with default counts and an empty issue list.
    fn new(path: PathBuf, relative_path: PathBuf, depth: usize) -> Self {
        Self {
            path,
            relative_path,
            depth,
            direct: Counts::default(),
            subtree: Counts::default(),
            child_dirs: 0,
            complete: true,
            issues: Vec::new(),
        }
    }
}

/// Owns the shared mutable state for a scan: options, emitted limit,
/// canonical root, frame stack, running summary, and the emit callback.
struct Aggregator<'a, F> {
    options: &'a ScanOptions,
    emitted_limit: usize,
    canonical_root: Option<PathBuf>,
    stack: Vec<Frame>,
    summary: ScanSummary,
    emit: F,
}

impl<'a, F: FnMut(&DirectoryRecord) -> io::Result<()>> Aggregator<'a, F> {
    /// Creates a new aggregator with the given options and emit callback.
    fn new(options: &'a ScanOptions, emit: F) -> Self {
        Self {
            options,
            emitted_limit: options.emitted_limit(),
            canonical_root: None,
            stack: Vec::new(),
            summary: ScanSummary::default(),
            emit,
        }
    }

    /// Dispatches a walk event to the appropriate handler.
    fn handle(&mut self, event: WalkEvent) -> io::Result<()> {
        match event {
            WalkEvent::Directory { path, depth: 0 } => self.on_root(path),
            WalkEvent::Directory { path, depth } => self.on_directory(path, depth),
            WalkEvent::File { path, depth, kind } => self.on_file(path, depth, kind),
            WalkEvent::Issue(issue) => self.on_issue(issue),
        }
    }

    /// Handles the root directory event: records the canonical root and
    /// pushes the root frame.
    fn on_root(&mut self, path: PathBuf) -> io::Result<()> {
        self.canonical_root = Some(path.clone());
        self.stack.push(Frame::new(path, PathBuf::new(), 0));
        self.summary.directories_scanned += 1;
        Ok(())
    }

    /// Handles a non-root directory event: flushes deeper frames, increments
    /// the parent's child count, and pushes a new frame if within limit.
    fn on_directory(&mut self, path: PathBuf, depth: usize) -> io::Result<()> {
        self.flush_while(&|f: &Frame| f.depth >= depth)?;
        if let Some(parent) = self.stack.last_mut() {
            parent.child_dirs += 1;
        }
        let should_push = depth <= self.emitted_limit || self.options.scope == Scope::Subtree;
        if should_push {
            let root = self.canonical_root.as_ref().unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(Path::new(""))
                .to_path_buf();
            self.stack.push(Frame::new(path, rel, depth));
            self.summary.directories_scanned += 1;
        }
        Ok(())
    }

    /// Handles a file event: flushes deeper frames and increments counts on
    /// the top frame if the file's parent matches it.
    fn on_file(&mut self, path: PathBuf, depth: usize, kind: Kind) -> io::Result<()> {
        self.flush_while(&|f: &Frame| f.depth >= depth)?;
        let parent = path.parent().map(Path::to_path_buf);
        let top = self
            .stack
            .last_mut()
            .filter(|top| parent.as_deref() == Some(top.path.as_path()));
        if let Some(top) = top {
            top.direct.increment(kind);
            top.subtree.increment(kind);
        }
        Ok(())
    }

    /// Handles an issue event: increments the error count, finds the target
    /// frame, flushes deeper frames, and attaches the issue.
    fn on_issue(&mut self, issue: ScanIssue) -> io::Result<()> {
        self.summary.errors += 1;
        let target_idx = self
            .stack
            .iter()
            .rposition(|frame| issue.path.starts_with(&frame.path))
            .unwrap_or(0);
        let target_depth = self.stack[target_idx].depth;
        self.flush_while(&|f: &Frame| f.depth > target_depth)?;
        let frame = self.stack.get_mut(target_idx).unwrap();
        frame.issues.push(issue);
        frame.complete = false;
        Ok(())
    }

    /// Flushes all remaining frames on the stack.
    fn flush_all(&mut self) -> io::Result<()> {
        self.flush_while(&|_f: &Frame| true)
    }

    /// Pops frames from the stack while the predicate holds on the top frame.
    fn flush_while(&mut self, pred: &impl Fn(&Frame) -> bool) -> io::Result<()> {
        match self.stack.last() {
            Some(top) if pred(top) => {
                let frame = self.stack.pop().unwrap();
                self.flush_frame(frame)?;
                self.flush_while(pred)
            }
            _ => Ok(()),
        }
    }

    /// Pops a single frame, merges its subtree into the parent, classifies it,
    /// and emits it if it passes the depth and kind filters.
    fn flush_frame(&mut self, frame: Frame) -> io::Result<()> {
        self.merge_into_parent(&frame);
        let counts = self.select_counts(&frame);
        let (kind, ratio) = classify_counts(&counts, frame.child_dirs, self.options.min_ratio);
        let record = self.build_record(frame, counts, kind, ratio);
        self.maybe_emit(&record, kind)?;
        Ok(())
    }

    /// Merges a flushed frame's subtree counts into its parent, propagating
    /// incompleteness in Subtree scope.
    fn merge_into_parent(&mut self, frame: &Frame) {
        let parent = self.stack.last_mut();
        if let Some(parent) = parent {
            parent.subtree.add(&frame.subtree);
            let propagate = self.options.scope == Scope::Subtree && !frame.complete;
            if propagate {
                parent.complete = false;
            }
        }
    }

    /// Selects the counts to use based on the configured scope.
    fn select_counts(&self, frame: &Frame) -> Counts {
        match self.options.scope {
            Scope::Direct => frame.direct.clone(),
            Scope::Subtree => frame.subtree.clone(),
        }
    }

    /// Builds a [`DirectoryRecord`] from a flushed frame and its classification.
    fn build_record(
        &self,
        frame: Frame,
        counts: Counts,
        kind: Kind,
        ratio: f64,
    ) -> DirectoryRecord {
        DirectoryRecord {
            path: frame.path,
            relative_path: frame.relative_path,
            depth: frame.depth,
            kind,
            counts,
            ratio,
            child_dirs: frame.child_dirs,
            complete: frame.complete,
            issues: frame.issues,
        }
    }

    /// Emits a record if it passes the depth and kind filters, and increments
    /// the emitted count.
    fn maybe_emit(&mut self, record: &DirectoryRecord, kind: Kind) -> io::Result<()> {
        let within_limit = record.depth <= self.emitted_limit;
        let kind_match = self.options.kinds.is_empty() || self.options.kinds.contains(&kind);
        let should_emit = within_limit && kind_match;
        if should_emit {
            (self.emit)(record)?;
            self.summary.directories_emitted += 1;
        }
        Ok(())
    }
}
