# Agent 约定

## 编译与验证：只走 GitHub Actions

本机性能不足，Rust 构建默认在 GitHub Actions 上跑（`.github/workflows/build.yml`，push `main` 或手动 `workflow_dispatch`），本地不保留 `target/`。

- **不要在本地运行** `cargo build` / `cargo check` / `cargo test` / `cargo clippy` / `cargo run`。
- **允许本地运行** 不产生构建产物的检查：`cargo fmt --all -- --check`、`git diff --check`、workflow YAML 解析、脚本/文档检查。
- 需要编译验证时：提交并 push（或 push 分支后 dispatch），读 Actions 结果；失败按日志修，不要在本地复现。
- 唯一例外：本地编译必须**同时**满足两个条件，否则交给 Actions：
  1. 相比 Action 往返能节省至少一半时间；
  2. 磁盘占用小于 3 GB。
  当前全量 `target/` 约 8.9 GB，因此默认视为不满足，仅允许极小的一次性原型（例如单个 crate 的玩具验证），用完立即删除。
- 发版仍只走 tag 触发的 Actions 流程，不要在本地打包或手动上传。
