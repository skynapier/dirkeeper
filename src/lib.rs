mod cli;
mod config;
mod filesystem;
mod model;
mod planning;
mod remote;
pub mod scan;
mod task;

/// CLI entry: answers an ssh askpass call, otherwise runs the command line.
pub fn run() -> std::process::ExitCode {
    if let Some(code) = crate::remote::ssh::maybe_askpass() {
        return code;
    }
    crate::cli::run()
}
