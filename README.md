# Decro Slint

Decro 的 Slint 原生 UI 便携版。复用既有 Rust 网易云核心（libnetease、扫码登录、歌单、音源），用 Slint 替代 WebView2，软件渲染，不依赖 WebView2 运行时。

## 当前功能

- 网易云扫码登录 / 网页 Cookie 导入 / 登录状态恢复，凭证使用 Windows DPAPI 加密
- 首页最近播放、推荐歌单、每日推荐、新歌速递
- 资料库歌单、最近播放
- 歌单详情（封面、简介）、歌曲搜索
- 真实歌单/歌曲封面，缓存到 `data/covers/`
- 原生音频播放（rodio）：播放/暂停、上一首/下一首、进度拖动、音量
- 音质选择：标准 / 较高 / 极高 / 无损 / Hi-Res
- 同步歌词、当前行高亮并自动居中跟随
- 设置页：账号信息、音质、日志级别、数据目录、打开日志目录
- 上次播放队列/曲目自动恢复，音量持久化
- 全部数据位于 exe 同目录 `data/`，便携升级只覆盖 exe

## data 目录

| 文件/目录 | 内容 |
| --- | --- |
| `netease-auth.bin` | DPAPI 加密的登录凭证 |
| `settings.json` | 音量、音质 |
| `last-playback.json` | 上次播放队列和索引 |
| `log-level` | 日志级别 |
| `logs/decro.log` | 运行日志 |
| `covers/` | 封面缓存 |

日志级别可选 `debug` / `info` / `warn` / `error` / `none`，也可以在设置页切换。

## 构建

```bash
cargo build --release
```

Windows 产物：`target/release/decro-slint.exe`。GitHub Actions 会打包成 `Decro.Slint_<version>_portable_x64.zip`。Windows 构建静态链接 CRT，不需要安装 VC++ 运行库。

## 已知限制

- 测试版，原生 UI 重写，不是 Tauri 版本的逐像素复刻
- 暂无桌面歌词、托盘、全局快捷键
- 歌词页自动跟随，暂不支持手动滚动歌词
- exe 未签名，Windows SmartScreen 可能提示
