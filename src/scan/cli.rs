// `dirkeeper scan` subcommand.
// 1. ScanArgs builds ScanOptions.
// 2. Output destination is validated before scanning.
// 3. Records are serialized as JSON array or JSONL.
// 4. Errors map to exit codes 0, 1, or 2.

use super::{DirectoryRecord, Kind, ScanError, ScanOptions, ScanSummary, Scope};
use clap::{Args, ValueEnum};
use std::io::{self, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use tempfile::NamedTempFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Json,
    Jsonl,
}

#[derive(Args, Debug)]
pub(crate) struct ScanArgs {
    #[arg(default_value = ".")]
    path: PathBuf,

    #[arg(short = 'R', long = "recursive")]
    recursive: bool,

    #[arg(short = 'a', long = "all")]
    include_hidden: bool,

    #[arg(long = "max-depth")]
    max_depth: Option<usize>,

    #[arg(long = "scope", value_enum, default_value = "direct")]
    scope: Scope,

    #[arg(long = "type", value_enum, value_delimiter = ',')]
    kinds: Vec<Kind>,

    #[arg(long = "min-ratio", default_value_t = 0.5)]
    min_ratio: f64,

    #[arg(long = "format", value_enum, default_value = "json")]
    format: OutputFormat,

    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    #[arg(long = "exclude")]
    exclude: Vec<PathBuf>,
}

impl ScanArgs {
    fn options(&self) -> ScanOptions {
        ScanOptions {
            recursive: self.recursive || self.max_depth.is_some(),
            max_depth: self.max_depth,
            include_hidden: self.include_hidden,
            scope: self.scope,
            min_ratio: self.min_ratio,
            kinds: self.kinds.clone(),
            exclude_paths: self.exclude.clone(),
        }
    }
}

// Lexically normalize a path, resolving `.` and `..` components.
// Unlike `fs::canonicalize`, this works on paths that do not exist yet.
fn normalize_path(path: &Path) -> PathBuf {
    path.components()
        .fold(PathBuf::new(), |mut acc, component| {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    acc.pop();
                }
                other => acc.push(other.as_os_str()),
            }
            acc
        })
}

// Validate the output path: reject symlinks and non-regular files.
fn validate_output_path(path: &Path) -> Result<(), ScanError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let is_symlink = meta.file_type().is_symlink();
            let is_regular = meta.is_file();
            if is_symlink || !is_regular {
                return Err(ScanError::Output(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("output path is not a regular file: {path:?}"),
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ScanError::Output(e)),
    }
}

// Serialize a single record for JSON array format.
fn write_json_record<W: Write>(
    writer: &mut W,
    first: &mut bool,
    record: &DirectoryRecord,
) -> io::Result<()> {
    if *first {
        writer.write_all(b"[")?;
        *first = false;
    } else {
        writer.write_all(b",")?;
    }
    serde_json::to_writer(&mut *writer, record).map_err(|e| {
        io::Error::new(
            e.io_error_kind().unwrap_or(io::ErrorKind::Other),
            e.to_string(),
        )
    })?;
    Ok(())
}

// Serialize a single record for JSONL format.
fn write_jsonl_record<W: Write>(writer: &mut W, record: &DirectoryRecord) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, record).map_err(|e| {
        io::Error::new(
            e.io_error_kind().unwrap_or(io::ErrorKind::Other),
            e.to_string(),
        )
    })?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

// Close a JSON array after all records have been written.
fn close_json_array<W: Write>(writer: &mut W, first: bool) -> Result<(), ScanError> {
    if first {
        writer.write_all(b"[]")?;
    } else {
        writer.write_all(b"]")?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

// Serialize scan results to a writer in the given format.
fn serialize_scan<W: Write>(
    root: &Path,
    options: &ScanOptions,
    format: OutputFormat,
    writer: &mut W,
) -> Result<ScanSummary, ScanError> {
    let mut first = true;
    let result = super::scan(root, options, |record: &DirectoryRecord| match format {
        OutputFormat::Json => write_json_record(writer, &mut first, record),
        OutputFormat::Jsonl => write_jsonl_record(writer, record),
    });
    let summary = result?;
    if format == OutputFormat::Json {
        close_json_array(writer, first)?;
    }
    writer.flush()?;
    Ok(summary)
}

// File output destination: spool file plus the parent and target paths.
struct FileOutput {
    spool: NamedTempFile,
    parent: PathBuf,
    target: PathBuf,
}

impl FileOutput {
    // Prepare file output: validate path, create spool, update exclude paths.
    fn prepare(target: &Path, options: &mut ScanOptions) -> Result<Self, ScanError> {
        validate_output_path(target)?;
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let cwd = std::env::current_dir().map_err(ScanError::Output)?;
        let report_path = normalize_path(&cwd.join(target));
        let spool = NamedTempFile::new().map_err(ScanError::Output)?;
        options.exclude_paths.push(report_path);
        options.exclude_paths.push(spool.path().to_path_buf());
        Ok(Self {
            spool,
            parent: parent.to_path_buf(),
            target: target.to_path_buf(),
        })
    }

    // Finalize file output: copy spool to a temp file in the parent, then persist.
    fn finalize(mut self) -> Result<(), ScanError> {
        self.spool
            .seek(io::SeekFrom::Start(0))
            .map_err(ScanError::Output)?;
        let mut final_tmp = NamedTempFile::new_in(&self.parent).map_err(ScanError::Output)?;
        io::copy(self.spool.as_file_mut(), final_tmp.as_file_mut()).map_err(ScanError::Output)?;
        final_tmp.as_file_mut().flush().map_err(ScanError::Output)?;
        final_tmp
            .as_file_mut()
            .sync_all()
            .map_err(ScanError::Output)?;
        final_tmp
            .persist(&self.target)
            .map_err(|e| ScanError::Output(e.error))?;
        Ok(())
    }
}

// Run the scan, handling output destination.
fn execute(args: &ScanArgs) -> Result<ScanSummary, ScanError> {
    let mut options = args.options();
    let summary = match args.output.as_deref() {
        None => {
            let stdout = io::stdout();
            let mut buf = io::BufWriter::new(stdout.lock());
            serialize_scan(&args.path, &options, args.format, &mut buf)?
        }
        Some(target) => {
            let mut output = FileOutput::prepare(target, &mut options)?;
            let summary = serialize_scan(
                &args.path,
                &options,
                args.format,
                output.spool.as_file_mut(),
            )?;
            output.finalize()?;
            summary
        }
    };
    Ok(summary)
}

// Map a successful scan summary to an exit code.
fn exit_from_summary(summary: &ScanSummary) -> ExitCode {
    if summary.errors == 0 {
        ExitCode::from(0)
    } else {
        eprintln!("{summary:?}");
        ExitCode::from(2)
    }
}

// Map an output error to an exit code, handling broken pipe on stdout.
fn exit_from_output_error(args: &ScanArgs, e: &io::Error) -> ExitCode {
    let is_broken_pipe = args.output.is_none() && e.kind() == io::ErrorKind::BrokenPipe;
    if is_broken_pipe {
        ExitCode::from(0)
    } else {
        eprintln!("{e}");
        ExitCode::from(1)
    }
}

// Subcommand entry point.
pub(crate) fn run(args: ScanArgs) -> ExitCode {
    match execute(&args) {
        Ok(summary) => exit_from_summary(&summary),
        Err(ScanError::Output(e)) => exit_from_output_error(&args, &e),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
