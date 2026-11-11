# Decro Slint

Decro 的 Slint 原生 UI 实验版。复用既有 Rust 网易云核心（libnetease、扫码登录、歌单、歌词、音源），用 Slint 替代 WebView2。

## 当前功能

- 网易云扫码登录 / 网页 Cookie 导入，登录状态 DPAPI 加密保存在 `data/netease-auth.bin`
- 首页最近播放、推荐歌单、新歌
- 资料库歌单/最近播放
- 歌单详情、歌曲搜索
- 原生音频播放（rodio）：播放/暂停、上一首/下一首、进度、音量
- 歌词列表与当前行高亮
- 全部数据位于 exe 同目录 `data/`，便携升级只覆盖 exe

## 构建

```bash
cargo build --release
```

Windows 产物：`target/release/decro-slint.exe`。GitHub Actions 会打包成 `Decro.Slint_<version>_portable_x64.zip`。
