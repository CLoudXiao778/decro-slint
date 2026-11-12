# Decro Slint — 交接

## 项目

- 工作目录：`/home/cloud/code/decro-slint`
- 仓库：`https://github.com/CLoudXiao778/decro-slint`，分支 `main`
- 当前版本：`0.3.6`（commit `270f4e2`，已发布预发布版 `v0.3.6-slint`）
- 最新发布：`https://github.com/CLoudXiao778/decro-slint/releases/tag/v0.3.6-slint`
  - 资产：`Decro.Slint_0.3.6_portable_x64.zip`
  - SHA256：`ec94dec1a921f80ecdce6f0eca772300c05950da6879b78611a60d93345d2769`
  - 预发布；SMTC/音频/歌词/morph 仍需 Windows 真机验证
- 定位：Tauri 版 Decro 的原生 Slint 重写，Windows x64 便携版。当前开始作为正式项目开发，不再是方案验证。
- Tauri 参考仓库：`/home/cloud/code/neteaseM3PC`（只读参考；本对话后续专注修 Slint，不要再把两者混在一起改）。

## 技术栈与硬约束

- Slint `1.18.1`，软件渲染器 `renderer-software`，不依赖 WebView2。
- 数据便携：exe 同目录 `data/`，升级只替换 exe。
- 目标：正常内存 <150MB、最小化 <75MB；保持低存储占用。
- 深色 UI 为主，中文界面；UI 按 Tauri 截图/CSS 重绘，不另起炉灶。
- 软件渲染器限制：没有实时 `backdrop-filter`；封面模糊是 `image` crate 预生成的真实模糊图；`drop-shadow` 在软件渲染器里是 TODO（不显示）。

## 已实现能力（0.3.5）

- 网易云扫码登录、Cookie 导入、登录态恢复（DPAPI）。
- 首页/推荐/资料库/搜索/歌单详情/最近播放/每日推荐。
- 搜索分类：歌曲/歌单/歌手/专辑（分类始终显示）。
- 歌手页、专辑页，专辑→歌手返回栈。
- 播放/暂停/上一首/下一首、进度恢复、音量、音质、喜欢、歌词与翻译。
- 播放页：dock↔播放页封面飞行、播放/下一首按钮 morph、波形进度条、队列面板。
- Windows SMTC：媒体键、系统媒体浮层、任务栏封面/状态。
- 关闭修复：X/Alt+F4 退出事件循环并结束进程（2 秒强制退出兜底）。
- 内存日志与最小化工作集回收（`target=memory`）。
- 性能优化：环境光预渲染、播放页背景去 1.16x 过采样、播放页展开后隐藏背后工作区。

## 代码地图

- `src/main.rs`：入口、UI 回调、播放器、恢复/保存、封面加载、morph、内存/退出逻辑。
- `ui/app.slint`：全部 UI、动画、播放页、列表、弹窗。
- `src/netease.rs` / `src/libnetease.rs` / `vendor/libnetease/`：网易云 API 与 C 封装。
- `src/media.rs`：Windows SMTC（独立线程 + 原子命令）。
- `src/memory.rs`：working set / private 统计与 `EmptyWorkingSet`。
- `src/auth.rs`、`src/state.rs`、`src/proxy.rs`、`src/logger.rs`、`src/paths.rs`、`src/secure_store.rs`。
- `assets/icons/`、`assets/wave.svg`、`assets/icon.png`。
- `Cargo.toml`、`build.rs`、`.github/workflows/build.yml`。
- `CHANGELOG.md`、`README.md`。

## 构建/发布

本地 Windows 目标检查（Linux 交叉）：

```bash
export PATH="/tmp/opencode/fakebin:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/tmp/opencode/cargo-target-slint-win
export CC_x86_64_pc_windows_gnu=gcc
export AR_x86_64_pc_windows_gnu=ar
cargo check --target x86_64-pc-windows-gnu
```

正式构建：push `main` 触发 GitHub Actions `slint-portable`，Windows MSVC 静态 CRT，产物为 `Decro.Slint_<version>_portable_x64.zip`。

发布流程：下载 Actions artifact → 校验/记录 SHA256 → `gh release create vX.Y.Z-slint ... --prerelease` 上传 zip → 立即取消 tag 触发的 workflow，避免覆盖手工资产。

版本需要同步：`Cargo.toml`、`ui/app.slint` 设置页 footer、`CHANGELOG.md`。

## 当前待办/风险

- **0.3.6 已提交并发布预发布版 `v0.3.6-slint`**：播放状态机、歌词滚动、设置持久化、登录、异步令牌、SMTC 进度、流式音频、搜索/错误处理等修改见 `CHANGELOG.md` 的 0.3.6 段；其余功能差距也在该段“仍待处理”中列出。真机验证后如需修复，继续在 0.3.6 基础上迭代。
- **0.3.5 尚未真机确认**：封面/播放按钮 morph 的落点、动画结束后的 1px 黑线、波形进度条、重启恢复错歌修复，都需要用户实机反馈。0.3.6 已进一步修正恢复进度被定时器擦除、旧音频不停止、歌词滚动方向等问题，仍需实机验证。
- **滚动帧率**：用户仍觉得未满 60fps。0.3.3 已做环境光预渲染/工作区隐藏/去 overscan；若仍卡，优先做长列表裁剪/虚拟化（资料库最近、最近播放、歌单详情、队列），并对比性能模式。
- **浅色主题未实现**：Tauri 有深/浅色；Slint 目前只有深色。
- **命令面板/键盘快捷键未实现**：Tauri 有 Ctrl+K、Space、方向键等。
- **功能审计剩余**：最近搜索建议、歌单置顶/使用频率排序、歌词搜索分类等。
- **SMTC/关闭退出/内存回收**已通过编译和 CI，但需要 Windows 真机验证。
- 恢复错歌：已按“解码后真实时长” clamp，未知时长不 seek；若仍复现，查 `data/logs/decro.log` 的 `target=restore`、`target=play`、`resume clamped`。

## 新会话建议顺序

1. `git pull`，先确认 0.3.5 各项实机反馈。
2. 关闭进程、morph 落点、1px 黑线、重启错歌逐项收尾。
3. 滚动性能：先定位具体页面，再做列表裁剪/虚拟化。
4. 正式路线：浅色主题 → 命令面板/快捷键 → Tauri 功能审计补齐。
5. 每次改动保持：软件渲染器、便携数据目录、内存目标、中文 UI、Tauri 视觉参考。
