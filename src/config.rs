// Runtime configuration for the remote command.
// 1. Load the TOML config file (if present).
// 2. Read environment variables.
// 3. Merge layers: CLI > env > file.
// 4. Resolve into RemoteSettings, validating required fields.

use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Keychain {
    pub(crate) service: String,
    pub(crate) account: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteSettings {
    pub(crate) host: String,
    pub(crate) bin: String,
    pub(crate) keychain: Option<Keychain>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteLayer {
    pub(crate) host: Option<String>,
    pub(crate) bin: Option<String>,
    pub(crate) keychain_service: Option<String>,
    pub(crate) keychain_account: Option<String>,
}

impl RemoteLayer {
    pub(crate) fn or(self, lower: RemoteLayer) -> RemoteLayer {
        RemoteLayer {
            host: self.host.or(lower.host),
            bin: self.bin.or(lower.bin),
            keychain_service: self.keychain_service.or(lower.keychain_service),
            keychain_account: self.keychain_account.or(lower.keychain_account),
        }
    }

    pub(crate) fn from_env() -> RemoteLayer {
        RemoteLayer {
            host: read_env("DIRKEEPER_REMOTE_HOST"),
            bin: read_env("DIRKEEPER_REMOTE_BIN"),
            keychain_service: read_env("DIRKEEPER_REMOTE_KEYCHAIN_SERVICE"),
            keychain_account: read_env("DIRKEEPER_REMOTE_KEYCHAIN_ACCOUNT"),
        }
    }

    pub(crate) fn resolve(self) -> Result<RemoteSettings, ConfigError> {
        let host = self.host.ok_or(ConfigError::Missing {
            key: "host",
            flag: "host",
            env: "DIRKEEPER_REMOTE_HOST",
        })?;
        let bin = self.bin.ok_or(ConfigError::Missing {
            key: "bin",
            flag: "remote-bin",
            env: "DIRKEEPER_REMOTE_BIN",
        })?;
        let keychain = match (self.keychain_service, self.keychain_account) {
            (Some(s), Some(a)) => Some(Keychain {
                service: s,
                account: a,
            }),
            (None, None) => None,
            _ => return Err(ConfigError::KeychainPair),
        };
        Ok(RemoteSettings {
            host,
            bin,
            keychain,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    remote: RemoteLayer,
}

pub(crate) fn config_path() -> Option<PathBuf> {
    std::env::var("DIRKEEPER_CONFIG")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("XDG_CONFIG_HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| PathBuf::from(s).join("dirkeeper").join("config.toml"))
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| {
                    PathBuf::from(s)
                        .join(".config")
                        .join("dirkeeper")
                        .join("config.toml")
                })
        })
}

pub(crate) fn load_file_layer() -> Result<RemoteLayer, ConfigError> {
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(RemoteLayer::default()),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RemoteLayer::default()),
        Err(e) => return Err(ConfigError::Read { path, source: e }),
    };
    let cfg: ConfigFile =
        toml::from_str(&content).map_err(|source| ConfigError::Parse { path, source })?;
    Ok(cfg.remote)
}

pub(crate) fn remote_settings(cli: RemoteLayer) -> Result<RemoteSettings, ConfigError> {
    cli.or(RemoteLayer::from_env())
        .or(load_file_layer()?)
        .resolve()
}

fn read_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error(
        "remote {key} not set: pass --{flag}, set {env}, or add {key} under [remote] in the config file"
    )]
    Missing {
        key: &'static str,
        flag: &'static str,
        env: &'static str,
    },
    #[error("keychain service and account must both be set or both absent")]
    KeychainPair,
    #[error("cannot read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_takes_upper_and_falls_back() {
        let upper = RemoteLayer {
            host: Some("upper-host".into()),
            bin: None,
            keychain_service: Some("svc".into()),
            keychain_account: None,
        };
        let lower = RemoteLayer {
            host: None,
            bin: Some("lower-bin".into()),
            keychain_service: None,
            keychain_account: Some("acct".into()),
        };
        let merged = upper.or(lower);
        assert_eq!(merged.host.as_deref(), Some("upper-host"));
        assert_eq!(merged.bin.as_deref(), Some("lower-bin"));
        assert_eq!(merged.keychain_service.as_deref(), Some("svc"));
        assert_eq!(merged.keychain_account.as_deref(), Some("acct"));
    }

    #[test]
    fn resolve_missing_host() {
        let layer = RemoteLayer {
            host: None,
            bin: Some("dirkeeper".into()),
            keychain_service: None,
            keychain_account: None,
        };
        let err = layer.resolve().unwrap_err();
        assert!(matches!(err, ConfigError::Missing { key: "host", .. }));
    }

    #[test]
    fn resolve_missing_bin() {
        let layer = RemoteLayer {
            host: Some("nas".into()),
            bin: None,
            keychain_service: None,
            keychain_account: None,
        };
        let err = layer.resolve().unwrap_err();
        assert!(matches!(err, ConfigError::Missing { key: "bin", .. }));
    }

    #[test]
    fn resolve_lone_keychain_service() {
        let layer = RemoteLayer {
            host: Some("nas".into()),
            bin: Some("dirkeeper".into()),
            keychain_service: Some("svc".into()),
            keychain_account: None,
        };
        let err = layer.resolve().unwrap_err();
        assert!(matches!(err, ConfigError::KeychainPair));
    }

    #[test]
    fn resolve_lone_keychain_account() {
        let layer = RemoteLayer {
            host: Some("nas".into()),
            bin: Some("dirkeeper".into()),
            keychain_service: None,
            keychain_account: Some("acct".into()),
        };
        let err = layer.resolve().unwrap_err();
        assert!(matches!(err, ConfigError::KeychainPair));
    }

    #[test]
    fn resolve_both_keychain() {
        let layer = RemoteLayer {
            host: Some("nas".into()),
            bin: Some("dirkeeper".into()),
            keychain_service: Some("svc".into()),
            keychain_account: Some("acct".into()),
        };
        let settings = layer.resolve().unwrap();
        assert_eq!(settings.host, "nas");
        assert_eq!(settings.bin, "dirkeeper");
        assert_eq!(
            settings.keychain,
            Some(Keychain {
                service: "svc".into(),
                account: "acct".into()
            })
        );
    }

    #[test]
    fn resolve_no_keychain() {
        let layer = RemoteLayer {
            host: Some("nas".into()),
            bin: Some("dirkeeper".into()),
            keychain_service: None,
            keychain_account: None,
        };
        let settings = layer.resolve().unwrap();
        assert_eq!(settings.keychain, None);
    }

    #[test]
    fn parse_full_remote_table() {
        let toml_str = r#"
[remote]
host = "nas"
bin = "dirkeeper"
keychain_service = "svc"
keychain_account = "acct"
"#;
        let cfg: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.remote.host.as_deref(), Some("nas"));
        assert_eq!(cfg.remote.bin.as_deref(), Some("dirkeeper"));
        assert_eq!(cfg.remote.keychain_service.as_deref(), Some("svc"));
        assert_eq!(cfg.remote.keychain_account.as_deref(), Some("acct"));
    }

    #[test]
    fn parse_empty_string_gives_default() {
        let cfg: ConfigFile = toml::from_str("").unwrap();
        assert_eq!(cfg.remote, RemoteLayer::default());
    }

    #[test]
    fn parse_unknown_key_is_error() {
        let toml_str = r#"
[remote]
unknown_field = "x"
"#;
        let result: Result<ConfigFile, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }
}
