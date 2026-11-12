# Decro Slint — 交接

## 项目

- 工作目录：`/home/cloud/code/decro-slint`
- 仓库：`https://github.com/CLoudXiao778/decro-slint`，分支 `main`
- 当前版本：`0.4.3`（commit `81fc72b`，已由 GitHub Actions 自动发布预发布版 `v0.4.3-slint`）
- 最新发布：`https://github.com/CLoudXiao778/decro-slint/releases/tag/v0.4.3-slint`
  - 资产：`Decro.Slint_0.4.3_portable_x64.zip`
  - SHA256：`c6a403c50bc4370e03365691033cd3e022b863d7836d5d1ffe04b2b15897569c`
  - 预发布；官方雷达歌单、首页回退、专辑/歌手链接仍需 Windows 真机验证
- 定位：Tauri 版 Decro 的原生 Slint 重写，Windows x64 便携版。当前开始作为正式项目开发，不再是方案验证。
- Tauri 参考仓库：`/home/cloud/code/neteaseM3PC`（只读参考；本对话后续专注修 Slint，不要再把两者混在一起改）。

## 技术栈与硬约束

- Slint `1.18.1`，默认 OpenGL（femtovg）硬件渲染，保留 `renderer-software` 作为无可用 GL 环境的自动回退；不依赖 WebView2。
- 数据便携：exe 同目录 `data/`，升级只替换 exe。
- 目标：正常内存 <150MB、最小化 <75MB；保持低存储占用。
- 中文界面；UI 按 Tauri 截图/CSS 重绘，不另起炉灶。0.4.0 起支持浅色/深色主题，播放页保持深色。
- 渲染限制：没有 WebView 式实时 `backdrop-filter`；封面模糊是 `image` crate 预生成的真实模糊图。

## 已实现能力（0.4.3）

- 网易云扫码登录、Cookie 导入、登录态恢复（DPAPI）。
- 首页/推荐/资料库/搜索/歌单详情/最近播放/每日推荐。
- 搜索分类：歌曲/歌单/歌手/专辑（分类始终显示）。
- 歌手页、专辑页，专辑→歌手返回栈。
- 播放/暂停/上一首/下一首、进度恢复、音量、音质、喜欢、歌词与翻译。
- 浅色/深色主题（标题栏/设置页切换，`settings.json` 持久化）；播放页/歌词页/队列固定深色。
- 资料库歌单置顶、打开次数排序、常用 8 个/展开全部（`data/playlist-state.json`）。
- 播放页更多菜单：查看歌手/专辑、复制歌曲信息、单曲音质覆盖（`data/track-quality.json`）。
- 详情页分享/更多、曲目菜单、歌单/每日推荐随机播放。
- 播放页：dock↔播放页封面飞行、播放/下一首按钮 morph、波形进度条、队列面板。
- Windows SMTC：媒体键、系统媒体浮层、任务栏封面/状态、拖动进度 seek。
- 任务栏缩略图按钮（上一首/播放暂停/下一首）与任务栏播放进度。
- 播放页专辑/多歌手内联链接、悬停下划线；多歌手分别打开详情，更多菜单按歌手列出。
- 关闭修复：X/Alt+F4 退出事件循环并结束进程（2 秒强制退出兜底）。
- 内存日志与最小化工作集回收（`target=memory`）。
- 性能优化：OpenGL 硬件渲染、环境光预渲染、播放页背景去 1.16x 过采样、播放页展开后隐藏背后工作区。

## 代码地图

- `src/main.rs`：入口、UI 回调、播放器、恢复/保存、封面加载、morph、内存/退出逻辑。
- `ui/app.slint`：全部 UI、动画、播放页、列表、弹窗。
- `src/netease.rs` / `src/libnetease.rs` / `vendor/libnetease/`：网易云 API 与 C 封装。
- `src/media.rs`：Windows SMTC + 任务栏缩略图/进度（独立线程 + 原子命令）+ 剪贴板文本。
- `src/memory.rs`：working set / private 统计与 `EmptyWorkingSet`。
- `src/auth.rs`、`src/state.rs`、`src/logger.rs`、`src/paths.rs`、`src/secure_store.rs`。
- `assets/icons/`、`assets/wave.svg`、`assets/icon.png`。
- `Cargo.toml`、`build.rs`、`.github/workflows/build.yml`。
- `CHANGELOG.md`、`README.md`。

## 构建/发布

- **只使用 GitHub Actions**（`.github/workflows/build.yml`），不在本地或 `/tmp` 做发布构建，不再手动 `gh release create`，也不再通过取消 tag workflow 来保护手工资产。
- push `main`：自动运行 `cargo fmt --all -- --check`、`cargo test --locked`、Windows MSVC release 构建，并上传 Actions artifact；不发布。
- push `vX.Y.Z-slint` tag：同样先格式检查、测试、构建；校验 tag 与 `Cargo.toml` 版本一致后，自动发布 prerelease，资产附带 `sha256.txt`，release notes 从 `CHANGELOG.md` 对应版本段生成。
- 发版步骤：同步 `Cargo.toml`、`ui/app.slint` footer、`CHANGELOG.md` → push `main` → 推送版本 tag。
- Actions 使用 Windows MSVC + 静态 CRT，产物 `Decro.Slint_<version>_portable_x64.zip`。
- Git 凭据由 OpenChamber 官方集成提供：`credential.https://github.com.helper = !/home/cloud/.config/openchamber/bin/git-credential-openchamber`；不再使用 `/tmp/opencode/bin/gh`，该二进制已删除。
- GitHub API/PR/Issue 优先走 OpenChamber 的 GitHub 集成（`github-auth.json` + `/api/source-control/github/*`）；发布由 Actions 完成，无 release API 也不需要 gh。

## 当前待办/风险

- **0.4.3 已自动发布**：首页大卡片和“雷达歌单”区使用官方 homepage block 雷达；普通推荐里的静态“欧美私人雷达”不再作为“为你推荐”大卡片。需真机确认雷达卡片加载和每日更新；雷达接口失败时会回退静态推荐。
- **Windows 11 任务栏**：如果系统不显示传统缩略图工具栏，任务栏上一首/播放暂停/下一首按钮不可见；进度条仍可用。Explorer 重启后按钮可能消失，当前未监听 `TaskbarButtonCreated` 重新添加。
- **性能**：已切换 OpenGL femtovg；若仍卡顿，先确认任务管理器 GPU 占用非 0，再排查长列表（资料库/最近播放/详情/搜索）虚拟化。
- **功能差距**：应用级快捷键/命令面板、窗口大小位置持久化、单实例唤回、桌面歌词/托盘。
- Cookie jar 与服务端 Set-Cookie 续期尚未全量同步到所有直连请求。
- 真机验证清单：浅色/深色主题、任务栏按钮/进度、SMTC seek、多歌手跳转、单曲音质重载、最小化 SMTC 播放暂停。

## 新会话建议顺序

1. `git pull`，确认 0.4.2 真机验证结果。
2. 性能：先确认 OpenGL 是否启用，再定位具体卡顿页面。
3. 发版必须走 GitHub Actions tag 流程，不要本地构建或手动上传。
4. 每次改动保持：默认 OpenGL + 软件回退、便携数据目录、内存目标、中文 UI、Tauri 视觉参考。
