// Remote command: config-driven host/bin, builds a remote argv, and runs it over ssh.
// 1. Resolve settings from CLI, env, and config file.
// 2. Validate the resolved remote binary.
// 3. Build the remote argv for the selected subcommand.
// 4. Execute the argv over ssh and return its exit code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use thiserror::Error;

use crate::config::{ConfigError, RemoteLayer, remote_settings};
use crate::remote::ssh::{SshError, SshOptions};

pub(crate) mod ssh;

/// CLI arguments for the `remote` subcommand.
#[derive(Args)]
pub(crate) struct RemoteArgs {
    /// Remote host (overrides DIRKEEPER_REMOTE_HOST and the config file)
    #[arg(long, global = true)]
    pub host: Option<String>,
    /// Remote binary path (overrides DIRKEEPER_REMOTE_BIN and the config file)
    #[arg(long, global = true)]
    pub remote_bin: Option<String>,
    /// Keychain service
    #[arg(long, global = true)]
    pub keychain_service: Option<String>,
    /// Keychain account
    #[arg(long, global = true)]
    pub keychain_account: Option<String>,
    #[command(subcommand)]
    pub command: RemoteCommands,
}

impl RemoteArgs {
    /// Build the CLI configuration layer from these arguments.
    fn layer(&self) -> RemoteLayer {
        RemoteLayer {
            host: self.host.clone(),
            bin: self.remote_bin.clone(),
            keychain_service: self.keychain_service.clone(),
            keychain_account: self.keychain_account.clone(),
        }
    }
}

/// Subcommands available under `remote`.
#[derive(clap::Subcommand)]
pub(crate) enum RemoteCommands {
    /// Probe remote system
    Probe,
    /// Plan moves
    Plan {
        /// Input mapping JSON file
        #[arg(long)]
        input: PathBuf,
        /// Task directory
        #[arg(long)]
        task_dir: String,
        /// Cleanup root paths
        #[arg(long)]
        cleanup_root: Vec<String>,
        /// Limit number of moves
        #[arg(long, value_parser = positive_usize)]
        limit: Option<usize>,
        /// Resolve existing destination conflicts
        #[arg(long)]
        resolve_existing: bool,
    },
    /// Apply moves
    Apply {
        /// Task directory
        #[arg(long)]
        task_dir: String,
        /// Max items to execute
        #[arg(long, value_parser = positive_usize)]
        max_items: Option<usize>,
    },
    /// Resume task execution
    Resume {
        /// Task directory
        #[arg(long)]
        task_dir: String,
        /// Max items to execute
        #[arg(long, value_parser = positive_usize)]
        max_items: Option<usize>,
    },
    /// Show task status
    Status {
        /// Task directory
        #[arg(long)]
        task_dir: String,
    },
    /// Cleanup task staging
    Cleanup {
        /// Task directory
        #[arg(long)]
        task_dir: String,
    },
}

/// Errors produced while building or executing a remote call.
#[derive(Debug, Error)]
pub(crate) enum RemoteError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    InvalidRemoteBin(&'static str),
    #[error("cannot read input file: {0}")]
    InputUnreadable(std::io::Error),
    #[error("input is not a regular file")]
    InputNotFile,
    #[error("invalid task-dir: {0}")]
    InvalidTaskDir(&'static str),
    #[error("invalid cleanup-root: {0}")]
    InvalidCleanupRoot(&'static str),
    #[error("{0}")]
    Ssh(#[from] SshError),
}

/// A remote call: the argv to run and the optional local file sent as stdin.
struct RemoteCall {
    argv: Vec<String>,
    input: Option<PathBuf>,
}

/// Parse a string as a positive `usize`.
fn positive_usize(s: &str) -> Result<usize, String> {
    s.parse::<usize>().map_err(|e| e.to_string()).and_then(|n| {
        Some(n)
            .filter(|&v| v > 0)
            .ok_or_else(|| "value must be greater than 0".to_string())
    })
}

/// Validate a remote path: non-empty, no NUL, absolute, no `.` or `..` segments.
fn validate_remote_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty path");
    }
    if path.contains('\0') {
        return Err("NUL in path");
    }
    if !path.starts_with('/') {
        return Err("path must be absolute");
    }
    if path
        .split('/')
        .filter(|c| !c.is_empty())
        .any(|c| c == "." || c == "..")
    {
        return Err("path must not contain . or ..");
    }
    Ok(())
}

/// Validate a remote binary name: non-empty, no NUL, no control characters.
fn validate_remote_bin(bin: &str) -> Result<(), &'static str> {
    if bin.is_empty() {
        return Err("empty remote-bin");
    }
    if bin.contains('\0') {
        return Err("NUL in remote-bin");
    }
    if bin.chars().any(|c| c.is_control()) {
        return Err("control characters in remote-bin");
    }
    Ok(())
}

/// Build the probe call: `/bin/sh -c` with the uname/HOME script.
fn build_probe() -> RemoteCall {
    let script = "uname -m; uname -r; printf '%s\\n' \"$HOME\"";
    RemoteCall {
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        input: None,
    }
}

/// Validate the local input file: must exist and be a regular file (not a symlink).
fn validate_local_input(input: &Path) -> Result<(), RemoteError> {
    let metadata = std::fs::symlink_metadata(input).map_err(RemoteError::InputUnreadable)?;
    if !metadata.is_file() {
        return Err(RemoteError::InputNotFile);
    }
    Ok(())
}

/// Build the plan call: validate input and paths, assemble the remote argv.
fn build_plan(
    bin: &str,
    input: &Path,
    task_dir: &str,
    cleanup_root: &[String],
    limit: Option<usize>,
    resolve_existing: bool,
) -> Result<RemoteCall, RemoteError> {
    validate_local_input(input)?;
    validate_remote_path(task_dir).map_err(RemoteError::InvalidTaskDir)?;
    cleanup_root
        .iter()
        .try_for_each(|root| validate_remote_path(root).map_err(RemoteError::InvalidCleanupRoot))?;

    let base = vec![
        bin.to_string(),
        "plan".to_string(),
        "--input".to_string(),
        "-".to_string(),
        "--task-dir".to_string(),
        task_dir.to_string(),
    ];
    let roots = cleanup_root
        .iter()
        .flat_map(|r| ["--cleanup-root".to_string(), r.clone()])
        .collect::<Vec<_>>();
    let limit_args = limit
        .into_iter()
        .flat_map(|l| ["--limit".to_string(), l.to_string()])
        .collect::<Vec<_>>();
    let resolve_args = resolve_existing
        .then(|| "--resolve-existing".to_string())
        .into_iter();
    let argv = base
        .into_iter()
        .chain(roots)
        .chain(limit_args)
        .chain(resolve_args)
        .collect();
    Ok(RemoteCall {
        argv,
        input: Some(input.to_path_buf()),
    })
}

/// Build an apply or resume call: validate task_dir, assemble the remote argv.
fn build_apply_resume(
    bin: &str,
    subcommand: &str,
    task_dir: &str,
    max_items: Option<usize>,
) -> Result<RemoteCall, RemoteError> {
    validate_remote_path(task_dir).map_err(RemoteError::InvalidTaskDir)?;
    let base = vec![
        bin.to_string(),
        subcommand.to_string(),
        "--task-dir".to_string(),
        task_dir.to_string(),
    ];
    let items_args = max_items
        .into_iter()
        .flat_map(|m| ["--max-items".to_string(), m.to_string()])
        .collect::<Vec<_>>();
    let argv = base.into_iter().chain(items_args).collect();
    Ok(RemoteCall { argv, input: None })
}

/// Build a status or cleanup call: validate task_dir, assemble the remote argv.
fn build_status_cleanup(
    bin: &str,
    subcommand: &str,
    task_dir: &str,
) -> Result<RemoteCall, RemoteError> {
    validate_remote_path(task_dir).map_err(RemoteError::InvalidTaskDir)?;
    let argv = vec![
        bin.to_string(),
        subcommand.to_string(),
        "--task-dir".to_string(),
        task_dir.to_string(),
    ];
    Ok(RemoteCall { argv, input: None })
}

/// Build the remote call for the given command and resolved binary.
fn build_call(command: RemoteCommands, bin: &str) -> Result<RemoteCall, RemoteError> {
    match command {
        RemoteCommands::Probe => Ok(build_probe()),
        RemoteCommands::Plan {
            input,
            task_dir,
            cleanup_root,
            limit,
            resolve_existing,
        } => build_plan(
            bin,
            &input,
            &task_dir,
            &cleanup_root,
            limit,
            resolve_existing,
        ),
        RemoteCommands::Apply {
            task_dir,
            max_items,
        } => build_apply_resume(bin, "apply", &task_dir, max_items),
        RemoteCommands::Resume {
            task_dir,
            max_items,
        } => build_apply_resume(bin, "resume", &task_dir, max_items),
        RemoteCommands::Status { task_dir } => build_status_cleanup(bin, "status", &task_dir),
        RemoteCommands::Cleanup { task_dir } => build_status_cleanup(bin, "cleanup", &task_dir),
    }
}

/// Resolve settings, validate the binary, build the call, and run it over ssh.
fn execute(args: RemoteArgs) -> Result<ExitCode, RemoteError> {
    let settings = remote_settings(args.layer())?;
    validate_remote_bin(&settings.bin).map_err(RemoteError::InvalidRemoteBin)?;
    let call = build_call(args.command, &settings.bin)?;
    let opts = SshOptions {
        host: settings.host,
        keychain: settings.keychain,
    };
    Ok(opts.run(&call.argv, call.input.as_deref())?)
}

/// Entry point for the `remote` subcommand.
pub(crate) fn run(args: RemoteArgs) -> ExitCode {
    match execute(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_remote_path_rejects_relative() {
        assert!(validate_remote_path("relative/path").is_err());
    }

    #[test]
    fn test_validate_remote_path_rejects_dot() {
        assert!(validate_remote_path("/path/./to/file").is_err());
    }

    #[test]
    fn test_validate_remote_path_rejects_dotdot() {
        assert!(validate_remote_path("/path/../to/file").is_err());
    }

    #[test]
    fn test_validate_remote_path_rejects_nul() {
        assert!(validate_remote_path("/path\0/to/file").is_err());
    }

    #[test]
    fn test_validate_remote_path_accepts_absolute() {
        assert!(validate_remote_path("/valid/absolute/path").is_ok());
    }

    #[test]
    fn test_positive_usize_valid() {
        assert_eq!(positive_usize("1").unwrap(), 1);
        assert_eq!(positive_usize("100").unwrap(), 100);
    }

    #[test]
    fn test_positive_usize_zero() {
        assert!(positive_usize("0").is_err());
    }

    #[test]
    fn test_positive_usize_invalid() {
        assert!(positive_usize("abc").is_err());
        assert!(positive_usize("-1").is_err());
    }
}
