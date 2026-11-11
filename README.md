# Decro Slint

Decro 的 Slint 原生 UI 便携版。界面按既有 Tauri 版重绘：自定义标题栏、图标侧栏、大卡片首页、悬浮播放 dock，以及带封面氛围背景的播放页/歌词页。不依赖 WebView2。

## 当前功能

- 网易云扫码登录 / 网页 Cookie 导入 / 登录状态恢复，凭证使用 Windows DPAPI 加密
- 按 Tauri 版重绘的应用外壳：自定义标题栏、窗口按钮、图标侧栏、页面头像
- 首页：每日推荐 / 推荐歌单大卡片，推荐歌单、新歌速递、最近播放横向封面架
- 资料库：登录卡片、我的歌单、最近播放
- 搜索：大号圆角搜索框、歌曲列表、歌单卡片
- 歌单详情：封面氛围 hero、大播放按钮、圆角歌曲行
- 播放 dock：Tauri 同款悬浮胶囊，封面/歌曲信息/播放/下一首/底部进度线
- 播放页：模糊封面背景、大封面、标题/歌手/专辑、进度与传输控制；点击封面或“打开歌词”进入歌词模式
- 歌词：大字号、当前行高亮、自动居中跟随；支持滚轮/拖动查看、点击歌词跳转，手动滚动后暂停跟随并可“回到当前”
- 真实歌单/歌曲封面，按行异步加载，缓存到 `data/covers/`
- 设置：账号、音质（标准/较高/极高/无损/Hi-Res）、日志级别（debug/info/warn/error/none）、数据目录
- 上次播放队列/曲目恢复，音量持久化
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

## 构建

```bash
cargo build --release
```

Windows 产物：`target/release/decro-slint.exe`。GitHub Actions 会打包成 `Decro.Slint_<version>_portable_x64.zip`。Windows 构建静态链接 CRT，不需要安装 VC++ 运行库。

## 已知限制

- 测试版，原生 UI 重绘，不是 Tauri 版的逐像素复刻
- 软件渲染器不支持实时 backdrop blur；播放页用封面缩略图放大模拟模糊氛围
- 暂无桌面歌词、托盘、全局快捷键
- exe 未签名，Windows SmartScreen 可能提示
