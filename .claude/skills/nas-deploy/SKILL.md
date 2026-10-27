---
name: nas-deploy
description: Build dirkeeper as a static Linux binary and install or update it on the NAS configured for `dirkeeper remote`, then verify; use when asked to deploy / update / remove dirkeeper on the NAS (部署到 NAS, 更新 NAS 上的 dirkeeper) or when a `remote` subcommand needs a newer NAS binary.
---

# NAS deploy

Scope: one static binary in the remote user's home; no root, no system dirs, no services.

## Rules

- Uploading, replacing or deleting on the NAS needs the user's OK in the current conversation; probes and read-only checks do not.
- Never read, print or log the keychain password; only the askpass helper touches it.
- Removing an old binary or state dir is a separate OK; first show what it contains (for a task dir, its `dirkeeper status`).
- No backup copies on the NAS (`.bak`, `.old`, dated copies).
- Keep hosts, accounts and keychain names out of the repo; they live only in the user's config.

## 1. Read config and probe

Read `host` and the keychain pair from the config first. Config file path: `$DIRKEEPER_CONFIG` if set, else `$XDG_CONFIG_HOME/dirkeeper/config.toml` if `XDG_CONFIG_HOME` is set, else `~/.config/dirkeeper/config.toml`. A missing file is not an error.

~~~sh
cat ~/.config/dirkeeper/config.toml
~~~

Run `dirkeeper remote probe` to get the NAS arch, kernel and `$HOME` (`<home>`).

~~~sh
dirkeeper remote probe
~~~

Target here is x86_64 Linux with an old kernel/glibc (Synology DSM class), hence a musl static build.

## 2. Login helper

Non-interactive ssh PATH on such NAS boxes is `/usr/bin:/bin:/usr/sbin:/sbin` and does not include `~/.local/bin`, so the config `bin` must be the full path.

Login when the config has a keychain pair: password-only ssh through `SSH_ASKPASS` using dirkeeper's own askpass mode.

Env:

~~~sh
SSH_ASKPASS=<path to local target/release/dirkeeper>
SSH_ASKPASS_REQUIRE=force
DISPLAY=dummy
DIRKEEPER_SSH_ASKPASS=1
DIRKEEPER_KEYCHAIN_SERVICE=<service>
DIRKEEPER_KEYCHAIN_ACCOUNT=<account>
~~~

~~~zsh
export SSH_ASKPASS=<path to local target/release/dirkeeper> \
  SSH_ASKPASS_REQUIRE=force \
  DISPLAY=dummy \
  DIRKEEPER_SSH_ASKPASS=1 \
  DIRKEEPER_KEYCHAIN_SERVICE=<service> \
  DIRKEEPER_KEYCHAIN_ACCOUNT=<account>
O=(-o PreferredAuthentications=password -o PubkeyAuthentication=no -o BatchMode=no)
~~~

ssh options: `-o PreferredAuthentications=password -o PubkeyAuthentication=no -o BatchMode=no`.

In zsh keep the options in an array (`O=(...)`, used as `"${O[@]}"`); a plain string is not word-split and ssh fails with "extra arguments at end of line".

Redirect stdin from `/dev/null` for one-shot ssh/scp.

Without a keychain pair, plain `ssh <host>` with the user's key setup.

## 3. Build

No musl target or cross linker is assumed on the Mac; build in docker:

~~~sh
docker run --rm --platform linux/amd64 \
  -v "$PWD":/src \
  -v dirkeeper-cargo-registry:/usr/local/cargo/registry \
  -w /src \
  rust:1.96-alpine \
  sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release --locked --target-dir target/nas-build'
~~~

Copy to `target/nas/dirkeeper-linux-x86_64`; check with `file` (expect `static-pie linked`).

~~~sh
mkdir -p target/nas && cp target/nas-build/release/dirkeeper target/nas/dirkeeper-linux-x86_64
file target/nas/dirkeeper-linux-x86_64
~~~

Also build the Mac `target/release/dirkeeper` (the askpass helper and the local client):

~~~sh
cargo build --release
~~~

First docker run pulls the image (~1 min), later builds 15–40 s.

## 4. Upload and swap

Upload with `scp -O` (legacy protocol; DSM-class sftp is unreliable) to `.local/bin/dirkeeper.new`. The directory is created first, then on the NAS `chmod 700`, `mv` over `dirkeeper` so an interrupted upload never leaves a broken binary, then `./dirkeeper --help`.

~~~sh
ssh "${O[@]}" <host> 'mkdir -p .local/bin' < /dev/null
scp -O "${O[@]}" target/nas/dirkeeper-linux-x86_64 <host>:.local/bin/dirkeeper.new < /dev/null
ssh "${O[@]}" <host> 'cd .local/bin && chmod 700 dirkeeper.new && mv dirkeeper.new dirkeeper && ls -la dirkeeper && ./dirkeeper --help | head -3' < /dev/null
~~~

## 5. Config

Set `bin = "<home>/.local/bin/dirkeeper"` (full path) in `~/.config/dirkeeper/config.toml`.

~~~toml
[remote]
host = "<host>"
bin = "<home>/.local/bin/dirkeeper"
keychain_service = "<service>"
keychain_account = "<account>"
~~~

## 6. Verify

- NAS `--help` shows the expected subcommands.
- One real read-only call from the Mac, either:
  - `dirkeeper remote status --task-dir <canonical home>/.local/state/dirkeeper/none` (the error must come from the remote binary), or
  - a small `dirkeeper remote scan <path> --max-depth 1 --format jsonl | wc -l`.
- Task dirs must be canonical paths (`realpath` on the NAS, since the home may be a symlink to a volume path).

## 7. Remove an old install

List first, delete only with OK.

~~~sh
ssh "${O[@]}" <host> 'ls -la .local/bin/dirkeeper; ls -la .local/state/dirkeeper/' < /dev/null
~~~

For a task dir, show its `dirkeeper status` before deleting.

## Report

- Install path, size and timestamp from `ls -la`.
- Config change.
- Verification command and result.
- Anything deleted.
