# AGENTS.md

通用目录重定位 CLI，覆盖目录分类（`scan`）与重定位，不含任何漫画概念；行为说明见 `docs/architecture.html`。

## 构建与测试
- `cargo build --release --locked`
- `cargo test --locked`
- `cargo clippy --all-targets -- -D warnings` 必须通过。

## 代码规则
- 编写和修改代码必须遵守 `STANDARD.md`。

## 安全约束
- 仅显式 `apply` 或 `resume` 可写入或删除 source。
- `scan` 只读文件元数据和扩展名，不读取内容，不移动、不重命名任何文件。
- 开发与测试仅使用临时目录；开发期间严禁写入 NAS。
- 严禁覆盖；严禁自动删除归属未证明的 staging 或 reservations。
- 不对任意网络文件系统声称掉电持久性；source 必须处于静止状态。
- 共享模型服务器保持原样，不得触碰。
- 项目代码编辑遵循用户全局 AGENTS 规则；Toolz 调用使用绝对仓库根路径。
- 除非用户要求，否则不 commit/push；不重复轮询；不转储 manifests。

## 文档
- 行为变更必须更新 `docs/architecture.html`。
- AGENTS.md 仅承载规则。
- 文档中禁止包含日志、历史记录、日期、测试结果或运行计数。
