# 代码标准

适用于本仓库全部 Rust 代码；`cargo clippy --all-targets -- -D warnings` 必须通过。

## 禁止 `for`/`while`/`loop`
遍历和循环逻辑一律使用 iterator adapters 或递归方法，禁止显式循环语句。

```rust
// 不要
let mut argv = base.to_vec();
for root in cleanup_root {
    argv.push("--cleanup-root".to_string());
    argv.push(root.clone());
}
if let Some(limit) = limit {
    argv.push("--limit".to_string());
    argv.push(limit.to_string());
}

// 要
let roots = cleanup_root
    .iter()
    .flat_map(|r| ["--cleanup-root".to_string(), r.clone()])
    .collect::<Vec<_>>();
let limit_args = limit
    .into_iter()
    .flat_map(|l| ["--limit".to_string(), l.to_string()])
    .collect::<Vec<_>>();
let argv = base.into_iter().chain(roots).chain(limit_args).collect();
```

```rust
// 不要
fn flush_while(&mut self, pred: &impl Fn(&Frame) -> bool) -> io::Result<()> {
    while let Some(top) = self.stack.last() {
        if !pred(top) { break; }
        let frame = self.stack.pop().unwrap();
        self.flush_frame(frame)?;
    }
    Ok(())
}

// 要
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
```

## 禁止嵌套 `if`
分支逻辑使用 `match`、`let ... else`、`?` 和组合子，禁止在 `if` 体内再写 `if`。

```rust
// 不要
let mut result = None;
if let Some(val) = self.value {
    if val > 0 {
        result = Some(val * 2);
    }
}

// 要
let result = self.value.filter(|&v| v > 0).map(|v| v * 2);
```

```rust
// 不要
if let Some(top) = self.stack.last() {
    if pred(top) {
        // ...
    }
}

// 要
match self.stack.last() {
    Some(top) if pred(top) => { /* ... */ }
    _ => Ok(()),
}
```

注意：使用 `then_some` 时，否定条件必须加括号，否则优先级会导致逻辑错误。
```rust
// 不要
!buf.is_empty().then_some(())?; // ! 作用在 ? 的结果上，不是 is_empty()

// 要
(!buf.is_empty()).then_some(())?;
```

## 有限取值集合用 enum
阶段、状态、类型等有限取值集合必须定义为 enum，禁止使用字符串常量或 `contains` 检查；JSON 格式通过 serde 属性保持不变。

```rust
// 不要
const STAGE_SCAN: &str = "scan";
const STAGE_APPLY: &str = "apply";
if stage == STAGE_SCAN { /* ... */ }

// 要
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stage {
    Scan,
    Apply,
}
```

## 具名 struct/enum，禁止匿名元组和魔法值
领域数据必须用具名 struct 或 enum 表达，禁止返回匿名元组或使用魔法值。

```rust
// 不要
let entry: (u64, String) = (0, "dir".to_string());
let output: (PathBuf, PathBuf, PathBuf) = (spool, parent, target);

// 要
#[derive(Serialize, Deserialize)]
#[serde(from = "(u64, String)", into = "(u64, String)")]
enum ManifestEntry {
    Dir,
    File { size: u64, hash: String },
}

struct FileOutput {
    spool: PathBuf,
    parent: PathBuf,
    target: PathBuf,
}
```

## 错误用 enum（`thiserror`），消息文字写在 `#[error(..)]`
错误类型必须定义为 enum 并实现 `thiserror::Error`，错误消息直接写在 `#[error(..)]` 属性中，调用者使用 `?` 和 `#[from]` 传播。

```rust
// 不要
fn resolve() -> Result<Settings, String> {
    if host.is_none() {
        return Err("host not set".to_string());
    }
    Ok(settings)
}

// 要
#[derive(Debug, Error)]
enum ConfigError {
    #[error("remote {key} not set: pass --{flag}, set {env}, or add {key} under [remote] in the config file")]
    Missing { key: &'static str, flag: &'static str, env: &'static str },
}

fn resolve(self) -> Result<RemoteSettings, ConfigError> {
    let host = self.host.ok_or(ConfigError::Missing {
        key: "host",
        flag: "host",
        env: "DIRKEEPER_REMOTE_HOST",
    })?;
    Ok(RemoteSettings { host, .. })
}
```

## 函数只做一件事，约 40 行以内
每个函数只负责一个单一职责，超过 40 行应拆分为多个小函数。

```rust
// 不要
fn walk(root: &Path, options: &ScanOptions, mut visit: impl FnMut(WalkEvent) -> io::Result<()>) -> Result<(), ScanError> {
    // 验证选项
    // 构建 walker
    // 遍历目录
    // 处理每个条目
    // 聚合结果
    // 返回 summary
    // ... (超过 40 行)
}

// 要
fn walk(root: &Path, options: &ScanOptions, mut visit: impl FnMut(WalkEvent) -> io::Result<()>) -> Result<(), ScanError> {
    let root = validate_root(root)?;
    let excluded = resolve_excluded(&root, options)?;
    build_walker(&root, options).into_iter()
        .filter_entry(|e| keep_entry(e, options, &excluded))
        .try_for_each(|entry| visit_entry(entry, &root, &mut visit))
        .map_err(ScanError::Output)
}
```

## 缩进不超过 3 层；共享可变状态做成 struct 加方法
函数体缩进不超过 3 层；共享可变状态的逻辑封装为 struct 并添加方法，事件处理 `match` 每个分支只调用一个方法。

```rust
// 不要
fn handle(&mut self, event: WalkEvent) -> io::Result<()> {
    if let WalkEvent::Directory { path, depth } = event {
        if depth == 0 {
            // 处理 root
        } else {
            // 处理 directory
        }
    } else if let WalkEvent::File { path, depth, kind } = event {
        // 处理 file
    }
    Ok(())
}

// 要
fn handle(&mut self, event: WalkEvent) -> io::Result<()> {
    match event {
        WalkEvent::Directory { path, depth: 0 } => self.on_root(path),
        WalkEvent::Directory { path, depth } => self.on_directory(path, depth),
        WalkEvent::File { path, depth, kind } => self.on_file(path, depth, kind),
        WalkEvent::Issue(issue) => self.on_issue(issue),
    }
}
```

## 重复的字面量和代码块提成构造函数或辅助函数
重复的 struct 初始化、参数列表或代码块应提取为构造函数或辅助函数。

```rust
// 不要
let frame = Frame {
    path: path.clone(),
    relative_path: rel.clone(),
    depth: depth,
    direct: Counts::default(),
    subtree: Counts::default(),
    child_dirs: 0,
    complete: true,
    issues: Vec::new(),
};

// 要
let frame = Frame::new(path, rel, depth);
```

```rust
// 不要
// 在多处重复构建 apply 和 resume 的 argv
let argv = vec![bin.to_string(), "apply".to_string(), "--task-dir".to_string(), task_dir.to_string()];
let argv = vec![bin.to_string(), "resume".to_string(), "--task-dir".to_string(), task_dir.to_string()];

// 要
let call = build_apply_resume(bin, "apply", task_dir, max_items)?;
let call = build_apply_resume(bin, "resume", task_dir, max_items)?;
```

## 运行配置来自配置文件和环境变量，代码里不写默认值
运行配置（如 ssh 主机、远端 binary 路径等）必须从配置文件和环境变量读取，代码中不硬编码默认值；使用层叠模式（CLI > env > file）合并配置。

```rust
// 不要
let host = std::env::var("DIRKEEPER_REMOTE_HOST").unwrap_or_else(|_| "localhost".to_string());

// 要
pub(crate) fn remote_settings(cli: RemoteLayer) -> Result<RemoteSettings, ConfigError> {
    cli.or(RemoteLayer::from_env())
        .or(load_file_layer()?)
        .resolve()
}
```

## 改动已有代码时，被改的函数一并改到符合标准
修改已有代码时，被修改的函数必须同时重构以符合本标准，不得保留违规代码。
