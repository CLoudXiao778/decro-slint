# libnetease

`go-musicfox/netease-music` v1.6.0 + 其 `netease-cli` 壳的 **C 语言移植**。
产物与 Go 版同名同协议：把 C 版 `netease-cli` 放到 netune 旁即可无缝替换，netune 零改动。

## 结构

```
include/netease/   公共头
src/vendor/        零依赖原语：AES-128 / MD5 / RSA(1024bit 无填充) / base64 / hex / 大数
                   / DEFLATE 解压（RFC 1951 + zlib/gzip 包装，用于响应体解码）
src/core/          请求内核：weapi/eapi 加密管线、Go 语义 JSON map、随机数、cookie jar
src/service/       API 端点：Go service/ 移植 + 探索页一族（2026-10 钉自 api-enhanced）
src/cli/           netease-cli 壳，stdout 协议 1:1 对齐 Go 版
tests/             向量测试 + verify_qr.py（独立 QR 解码校验）+ dualrun.py（差分对照，40 用例）
```

## 构建 / 测试

```sh
./tests/run_tests.sh          # 重新生成向量 + cmake 构建 + ctest
```

依赖：CMake ≥ 3.16、libcurl。

## 风控应对（可选加固，默认全部关闭，零行为变更）

网易云对逆向 API 的风控信号（-460/-462/8821/空 body/高频限流等）与分层应对策略，
见 **[docs/RISKS.md](docs/RISKS.md)**（基于 2026-08 活跃上游 api-enhanced / chaunsin / Meting-API 调研）。

可选开关（环境变量或 C API，见 `include/netease/risk.h` / `http.h` / `request.h`）：

| 变量 | 作用 |
|---|---|
| `NE_REAL_IP=<国内IP>` | 注入 `X-Real-IP`+`X-Forwarded-For`（海外/数据中心 IP 风控的常见解药） |
| `NE_RANDOM_CN_IP=1` | 未设 NE_REAL_IP 时每请求自动生成国内 IP |
| `NE_RATE_LIMIT_MS` / `NE_RATE_LIMIT_JITTER_MS` | 请求前随机间隔（真人节奏） |
| `NE_UA_ROTATE=1` | PC 浏览器 UA 轮换 |
| `NE_RETRY_RISK=<n>` | 对 -460/高频/传输错误指数退避重试（-462 不自动重试） |
| `NE_NO_KEEPALIVE=1` | 关连接复用（配合代理换 IP 生效） |
| `NE_BROWSER_HEADERS=1` | 补浏览器标准请求头（Accept/Accept-Language/Sec-Fetch 系） |
| `NE_HTTP2=1` | 显式协商 HTTP/2（浏览器标配，需 libcurl 带 nghttp2） |

常用组合（低风险读接口）：`NE_RATE_LIMIT_MS=800 NE_RATE_LIMIT_JITTER_MS=400 NE_RETRY_RISK=3 NE_UA_ROTATE=1`，
数据中心出口加 `NE_RANDOM_CN_IP=1`，进阶加 `NE_BROWSER_HEADERS=1`；
TLS 指纹层（curl-impersonate / LD_PRELOAD）接入见 docs/RISKS.md 第八节。

## 命令一览（与 Go 版 33 命令一一对应）

| 类别 | 命令 |
|------|------|
| 搜索/歌曲 | `search` `search-pl` `check-music` `song-url` `song-detail` `lyric` |
| 歌单 | `playlist` `playlist-tracks` `user-playlist` `playlists` `toplist` `recommend-resource` `recommend-songs` `recommend-playlists` `record-recent` |
| 红心 | `liked` `liked-check` `like` |
| 登录 | `qr-key` `qr-check` `qr-render` `qr-image` `login-email` `login-cellphone` `login-refresh` `login-status` |
| 写操作 | `subscribe` `track-add` `track-del` `playlist-create` `playlist-rename` `playlist-delete` `account-name` |

## 差分对照（dualrun.py）

```sh
# C 单跑：40 用例 = 33 命令正路径 + 5 错误路径（退出码/stderr 文本）
python3 tests/dualrun.py --cli build/netease-cli

# 双跑：同参数、同 HOME、同 stub 服务器，与 Go 版逐字节比对
# （唯一归一化项：qr-key 输出里 chainId 的毫秒时间戳）
python3 tests/dualrun.py --cli build/netease-cli --go /path/to/go/netease-cli
```

stub 服务器同时校验三条加密通道的请求侧：weapi 外层 AES-CBC 解密、
linuxapi `eparams` 全解密按内层 url 分发、eapi 全解密 + md5 信封校验。

## 退出码 / stderr 语义（与 Go 版一致）

- 无参数 → stderr `usage: netease-cli <cmd> [args...]`，exit 1
- 未知命令 → stderr `unknown cmd: <cmd>`，exit 1
- 参数缺失 → 各命令 usage 文本，exit 1
- JSON 解析失败 → Go `encoding/json` 逐字错误文本，如
  `parse account failed: invalid character '<' looking for beginning of value`

## Windows

- 主目录取 `%USERPROFILE%`（等价 `os.UserHomeDir()`），cookie 路径
  `%USERPROFILE%\.cache\netune\cookies.txt`
- 无 POSIX 专用调用；CMake + libcurl + zlib 可用 MSVC/MinGW 构建
- **已实证可用**：`build_ninja.sh`（CMake "Ninja" 生成器 + cl.exe + vcpkg）完整跑通
  `configure → build → ctest`，5/5 全绿（crypto / jval / rewrite / qr 单测 + dualrun 52 用例）。
  前置：cl.exe 环境变量（INCLUDE/LIB，脚本里已处理）、vcpkg（curl）、ninja、带 pycryptodome
  的隔离 venv（`ref_impl.py` 生成 `expected.h` 向量）。**不需要** MSBuild / .sln。
- 两个坑（都踩过）：
  - CMake 的 **"Visual Studio" 生成器在本机崩溃** —— 它调 MSBuild 探测 `VCTargetsPath` 时
    MSBuild 报 Access violation（疑似该 VS 实例自身问题）。这不影响构建本身，
    Ninja / NMake / 直编都绕开它；要用 VS 生成器得先单独排查 MSBuild。
  - `build_check.sh`（cl.exe 直编全部 .c 的快速语法检查）**必须带 `NE_HAVE_CURL` / `NE_HAVE_ZLIB`
    宏**，与真实构建同配置。不带宏会漏检 `http.c` 的声明顺序 bug —— 即「检查通过但真构建挂」。
    另：在 Git Bash 里跑要 `MSYS_NO_PATHCONV=1 bash build_check.sh`，否则 cl 的 `/nologo`、`/W4`
    会被当路径吃掉，假报一堆 `C1083 无法打开包括文件`。
  - 覆盖范围：`build_check.sh` 只编 libnetease 自己的源，**不覆盖宿主的 JNI/绑定层**。
