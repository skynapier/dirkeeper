//! Runs a remote argv over `/usr/bin/ssh` and, when a keychain is configured,
//! acts as its own `SSH_ASKPASS` helper that reads the password via
//! `/usr/bin/security`.
//!
//! 1. Build the ssh command line (fixed options, auth mode, host, quoted argv).
//! 2. Validate the host and, when present, the keychain.
//! 3. Spawn, wait, and map the exit status to an `ExitCode`.
//! 4. When invoked as the askpass helper, fetch the password from the keychain.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::config::Keychain;

/// Options for a single remote ssh invocation.
#[derive(Clone, Debug)]
pub(crate) struct SshOptions {
    pub(crate) host: String,
    pub(crate) keychain: Option<Keychain>,
}

/// Errors produced while building, running, or validating an ssh invocation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SshError {
    #[error("invalid host")]
    InvalidHost,
    #[error("invalid keychain {0}")]
    InvalidKeychain(&'static str),
    #[error("keychain mode requires macOS")]
    KeychainRequiresMacos,
    #[error("no remote command")]
    NoCommand,
    #[error("NUL in remote argument")]
    NulInArgument,
    #[error("cannot resolve current_exe")]
    CurrentExe,
    #[error("open input: {0}")]
    OpenInput(std::io::Error),
    #[error("spawn ssh: {0}")]
    Spawn(std::io::Error),
    #[error("wait ssh: {0}")]
    Wait(std::io::Error),
    #[error("ssh terminated by signal")]
    Signal,
}

/// Single-quote a value for safe inclusion in a remote shell command.
pub(crate) fn shell_quote(value: &str) -> String {
    let quoted = value.chars().fold(String::new(), |mut acc, ch| {
        if ch == '\'' {
            acc.push_str("'\\''");
        } else {
            acc.push(ch);
        }
        acc
    });
    format!("'{quoted}'")
}

/// Join the remote arguments into a single shell command string.
fn remote_command(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

impl SshOptions {
    /// Validate the host and, when present, the keychain.
    pub fn validate(&self) -> Result<(), SshError> {
        if self.host.is_empty()
            || self.host.starts_with('-')
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(SshError::InvalidHost);
        }
        self.keychain.as_ref().map_or(Ok(()), validate_keychain)
    }

    /// Build the fixed ssh options that precede the auth mode.
    fn base_command() -> Command {
        let mut cmd = Command::new("/usr/bin/ssh");
        cmd.arg("-T")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes")
            .arg("-o")
            .arg("ConnectTimeout=15")
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("NumberOfPasswordPrompts=1");
        cmd
    }

    /// Apply the auth options and environment for the given keychain mode.
    fn apply_auth(cmd: &mut Command, keychain: Option<&Keychain>) -> Result<(), SshError> {
        match keychain {
            Some(keychain) => {
                cmd.arg("-o").arg("PreferredAuthentications=password");
                cmd.arg("-o").arg("PubkeyAuthentication=no");
                cmd.arg("-o").arg("BatchMode=no");
                cmd.env_remove("DIRKEEPER_SSH_ASKPASS");
                cmd.env_remove("DIRKEEPER_KEYCHAIN_SERVICE");
                cmd.env_remove("DIRKEEPER_KEYCHAIN_ACCOUNT");
                cmd.env_remove("SSH_ASKPASS");
                cmd.env_remove("SSH_ASKPASS_REQUIRE");
                cmd.env(
                    "SSH_ASKPASS",
                    std::env::current_exe().map_err(|_| SshError::CurrentExe)?,
                );
                cmd.env("SSH_ASKPASS_REQUIRE", "force");
                if std::env::var_os("DISPLAY").is_none() {
                    cmd.env("DISPLAY", "dummy");
                }
                cmd.env("DIRKEEPER_SSH_ASKPASS", "1");
                cmd.env("DIRKEEPER_KEYCHAIN_SERVICE", &keychain.service);
                cmd.env("DIRKEEPER_KEYCHAIN_ACCOUNT", &keychain.account);
            }
            None => {
                cmd.arg("-o").arg("BatchMode=yes");
            }
        }
        Ok(())
    }

    /// Run the remote command and return its exit code.
    pub fn run(&self, arguments: &[String], input: Option<&Path>) -> Result<ExitCode, SshError> {
        self.validate()?;
        if arguments.is_empty() {
            return Err(SshError::NoCommand);
        }
        if arguments.iter().any(|arg| arg.contains('\0')) {
            return Err(SshError::NulInArgument);
        }
        let mut cmd = Self::base_command();
        Self::apply_auth(&mut cmd, self.keychain.as_ref())?;
        cmd.arg("--").arg(&self.host);
        cmd.arg(remote_command(arguments));
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        match input {
            Some(path) => {
                let file = std::fs::File::open(path).map_err(SshError::OpenInput)?;
                cmd.stdin(Stdio::from(file));
            }
            None => {
                cmd.stdin(Stdio::null());
            }
        }
        let mut child = cmd.spawn().map_err(SshError::Spawn)?;
        let status = child.wait().map_err(SshError::Wait)?;
        match status.code() {
            Some(code) => Ok(ExitCode::from(code as u8)),
            None => Err(SshError::Signal),
        }
    }
}

/// Validate a keychain entry: service, then account, then the macOS gate.
fn validate_keychain(keychain: &Keychain) -> Result<(), SshError> {
    if keychain.service.is_empty() || keychain.service.chars().any(|c| c.is_control()) {
        return Err(SshError::InvalidKeychain("service"));
    }
    if keychain.account.is_empty() || keychain.account.chars().any(|c| c.is_control()) {
        return Err(SshError::InvalidKeychain("account"));
    }
    if !cfg!(target_os = "macos") {
        return Err(SshError::KeychainRequiresMacos);
    }
    Ok(())
}

/// Whether a prompt string asks for a password (not a passphrase or host key).
fn is_password_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    lower.contains("password")
        && !lower.contains("passphrase")
        && !lower.contains("host authenticity")
        && !lower.contains("are you sure you want to continue connecting")
}

/// Read a non-empty, control-char-free environment variable.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty() && !value.chars().any(|c| c.is_control()))
}

/// Fetch the password from the keychain and write it to stdout.
fn askpass() -> Option<()> {
    cfg!(target_os = "macos").then_some(())?;
    (!std::io::stdout().is_terminal()).then_some(())?;
    let args = std::env::args().collect::<Vec<_>>();
    args[1..]
        .iter()
        .any(|a| is_password_prompt(a))
        .then_some(())?;
    let service = env_value("DIRKEEPER_KEYCHAIN_SERVICE")?;
    let account = env_value("DIRKEEPER_KEYCHAIN_ACCOUNT")?;
    let mut cmd = Command::new("/usr/bin/security");
    cmd.arg("find-generic-password")
        .arg("-s")
        .arg(&service)
        .arg("-a")
        .arg(&account)
        .arg("-w")
        .env_remove("DIRKEEPER_SSH_ASKPASS")
        .env_remove("DIRKEEPER_KEYCHAIN_SERVICE")
        .env_remove("DIRKEEPER_KEYCHAIN_ACCOUNT")
        .env_remove("SSH_ASKPASS")
        .env_remove("SSH_ASKPASS_REQUIRE")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut buf = Vec::new();
    let mut stdout = child.stdout.take()?;
    stdout.read_to_end(&mut buf).ok()?;
    child.wait().ok()?.success().then_some(())?;
    (!buf.is_empty()).then_some(())?;
    std::io::stdout().write_all(&buf).ok()
}

/// Entry point for the askpass helper; `None` unless invoked as such.
pub(crate) fn maybe_askpass() -> Option<ExitCode> {
    if std::env::var_os("DIRKEEPER_SSH_ASKPASS") != Some("1".into()) {
        return None;
    }
    Some(askpass().map_or(ExitCode::from(1), |_| ExitCode::SUCCESS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keychain;

    fn opts(host: &str, keychain: Option<Keychain>) -> SshOptions {
        SshOptions {
            host: host.into(),
            keychain,
        }
    }

    #[test]
    fn test_shell_quote() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("$(x)"), "'$(x)'");
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn test_validate_host() {
        assert!(opts("host", None).validate().is_ok());
        assert!(opts("", None).validate().is_err());
        assert!(opts("-x", None).validate().is_err());
        assert!(opts("a b", None).validate().is_err());
    }

    #[test]
    fn test_validate_credentials() {
        let empty_service = Keychain {
            service: "".into(),
            account: "a".into(),
        };
        assert!(opts("h", Some(empty_service)).validate().is_err());
        let control_account = Keychain {
            service: "s".into(),
            account: "a\nb".into(),
        };
        assert!(opts("h", Some(control_account)).validate().is_err());
    }

    #[test]
    fn test_is_password_prompt() {
        assert!(is_password_prompt("user@host's password: "));
        assert!(is_password_prompt("password: "));
        assert!(is_password_prompt("Password: "));
        assert!(!is_password_prompt("passphrase: "));
        assert!(!is_password_prompt(
            "Enter passphrase for key '/home/u/.ssh/id_rsa': "
        ));
        assert!(!is_password_prompt(
            "The authenticity of host 'host' (1.2.3.4) can't be established."
        ));
        assert!(!is_password_prompt(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        ));
        assert!(!is_password_prompt(""));
    }
}
