use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::remote;
use crate::task::engine as task_engine;
use crate::task::model::TaskSummary;

#[derive(Parser)]
#[command(name = "dirkeeper")]
#[command(about = "Generic relocation CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Classify directories by file type
    Scan(crate::scan::cli::ScanArgs),
    /// Plan moves from input mapping
    Plan {
        /// Input mapping JSON file
        #[arg(short, long)]
        input: PathBuf,
        /// Task directory
        #[arg(long)]
        task_dir: PathBuf,
        /// Cleanup root paths (repeatable)
        #[arg(long)]
        cleanup_root: Vec<PathBuf>,
        /// Limit number of moves
        #[arg(long)]
        limit: Option<usize>,
        /// Resolve existing destination conflicts
        #[arg(long)]
        resolve_existing: bool,
        /// Create a missing destination parent (one level) during apply
        #[arg(long)]
        create_parents: bool,
    },
    /// Apply moves from plan
    Apply {
        /// Task directory
        #[arg(long)]
        task_dir: PathBuf,
        /// Max items to execute
        #[arg(long)]
        max_items: Option<usize>,
    },
    /// Resume task execution
    Resume {
        /// Task directory
        #[arg(long)]
        task_dir: PathBuf,
        /// Max items to execute
        #[arg(long)]
        max_items: Option<usize>,
    },
    /// Show task status
    Status {
        /// Task directory
        #[arg(long)]
        task_dir: PathBuf,
    },
    /// Cleanup task staging
    Cleanup {
        /// Task directory
        #[arg(long)]
        task_dir: PathBuf,
    },
    /// Execute native remote task over SSH
    Remote(crate::remote::RemoteArgs),
}

fn print_summary(summary: &TaskSummary) -> ExitCode {
    let json = serde_json::to_string(summary).unwrap_or_else(|_| "{}".to_string());
    println!("{}", json);

    if summary.conflicts > 0 || summary.errors > 0 {
        ExitCode::FAILURE
    } else if summary.pending > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::SUCCESS
    }
}

pub(crate) fn run() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan(args) => crate::scan::cli::run(args),
        Commands::Plan {
            input,
            task_dir,
            cleanup_root,
            limit,
            resolve_existing,
            create_parents,
        } => {
            let limit_val = limit.unwrap_or(0);
            if limit.is_some() && limit_val == 0 {
                eprintln!("Error: --limit must be a positive number");
                return ExitCode::FAILURE;
            }
            let limit_opt = if limit.is_some() {
                Some(limit_val)
            } else {
                None
            };
            match task_engine::prepare(
                &input,
                &task_dir,
                &cleanup_root,
                limit_opt,
                resolve_existing,
                create_parents,
            ) {
                Ok(summary) => print_summary(&summary),
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Apply {
            task_dir,
            max_items,
        } => {
            let max_items_val = max_items.unwrap_or(0);
            if max_items.is_some() && max_items_val == 0 {
                eprintln!("Error: --max-items must be a positive number");
                return ExitCode::FAILURE;
            }
            let max_items_opt = if max_items.is_some() {
                Some(max_items_val)
            } else {
                None
            };
            match task_engine::execute(&task_dir, max_items_opt) {
                Ok(summary) => print_summary(&summary),
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Resume {
            task_dir,
            max_items,
        } => {
            let max_items_val = max_items.unwrap_or(0);
            if max_items.is_some() && max_items_val == 0 {
                eprintln!("Error: --max-items must be a positive number");
                return ExitCode::FAILURE;
            }
            let max_items_opt = if max_items.is_some() {
                Some(max_items_val)
            } else {
                None
            };
            match task_engine::execute(&task_dir, max_items_opt) {
                Ok(summary) => print_summary(&summary),
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Commands::Status { task_dir } => match task_engine::status(&task_dir) {
            Ok(summary) => print_summary(&summary),
            Err(e) => {
                eprintln!("Error: {}", e);
                ExitCode::FAILURE
            }
        },
        Commands::Cleanup { task_dir } => match task_engine::cleanup(&task_dir) {
            Ok(summary) => print_summary(&summary),
            Err(e) => {
                eprintln!("Error: {}", e);
                ExitCode::FAILURE
            }
        },
        Commands::Remote(args) => remote::run(args),
    }
}
