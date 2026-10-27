# dirkeeper

A CLI that classifies directories (`scan`) and moves directories with a resumable, evidence-keeping task (`plan` / `apply` / `resume` / `status` / `cleanup`), locally or on a remote host over ssh (`remote`).

See [docs/architecture.html](docs/architecture.html) for behavior details.

## Build

~~~sh
cargo build --release
~~~

The binary is `target/release/dirkeeper`.

## Usage

~~~sh
dirkeeper scan /data/library -R --format jsonl
dirkeeper plan --input /data/library/mapping.json --task-dir /data/tasks/t1
dirkeeper apply --task-dir /data/tasks/t1
dirkeeper resume --task-dir /data/tasks/t1
dirkeeper status --task-dir /data/tasks/t1
dirkeeper cleanup --task-dir /data/tasks/t1
dirkeeper remote probe
dirkeeper remote scan /data/library -R --format jsonl > library.jsonl
dirkeeper remote plan --input /data/library/mapping.json --task-dir /data/tasks/t1
dirkeeper remote apply --task-dir /data/tasks/t1
dirkeeper remote resume --task-dir /data/tasks/t1
dirkeeper remote status --task-dir /data/tasks/t1
dirkeeper remote cleanup --task-dir /data/tasks/t1
~~~

## Remote configuration

Remote settings are resolved from three layers, highest priority first: CLI flags, then the environment variables `DIRKEEPER_REMOTE_HOST` / `DIRKEEPER_REMOTE_BIN` / `DIRKEEPER_REMOTE_KEYCHAIN_SERVICE` / `DIRKEEPER_REMOTE_KEYCHAIN_ACCOUNT`, then the TOML config file.

The config file is `$DIRKEEPER_CONFIG` if set, otherwise `$XDG_CONFIG_HOME/dirkeeper/config.toml` if `XDG_CONFIG_HOME` is set, otherwise `~/.config/dirkeeper/config.toml`. A missing file is not an error.

~~~toml
[remote]
host = "user@nas.local"
bin = "dirkeeper"
~~~

The macOS keychain pair `keychain_service` / `keychain_account` is optional and must be set together.

## License

Dual-licensed under MIT or Apache-2.0, at your option; see `LICENSE-MIT` and `LICENSE-APACHE`.
