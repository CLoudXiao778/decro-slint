# 网易云音乐逆向 API 风控风险与应对（libnetease 加固说明）

> 调研时间：2026-08。覆盖上游：`NeteaseCloudMusicApiEnhanced/api-enhanced`（Node，2026-08 仍活跃）、
> `chaunsin/netease-cloud-music`（Go，2026-08 仍活跃）、`xizeyoupan/Meting-API`（JS）、
> `go-musicfox/netease-music`（Go，libnetease 移植源）、原版 `Binaryify/NeteaseCloudMusicApi`（**已于 2024-02 归档清空**，
> 仅存 README，其文档与"realIP 头"方案仍被所有活跃 fork 继承）、`mos9527/pyncm`（**已从 GitHub/PyPI 下架**）。

---

## 一、上游现状（先认清环境）

| 项目 | 语言 | 状态 | 与 libnetease 的关系 |
|---|---|---|---|
| Binaryify/NeteaseCloudMusicApi | Node | 2024-02 归档清空（仅 README） | 事实上的"祖源"，realIP/X-Real-IP 方案出处 |
| NeteaseCloudMusicApiEnhanced/api-enhanced | Node | **活跃（2026-08-27 仍有提交）** | 活跃度最高 fork，NMTID 下发修复等最新实践来源 |
| chaunsin/netease-cloud-music | Go | **活跃（2026-08 仍有提交）** | 风控警告与登录方式取舍的权威参考 |
| xizeyoupan/Meting-API | JS | 活跃 | 随机 UA/随机国内 IP/双头注入 |
| go-musicfox/netease-music | Go | 2026-04 仍有提交（captcha 登录修复） | libnetease 的移植源（锚定 v1.6.0） |
| mos9527/pyncm | Python | **已下架** | 侧面印证逆向项目生存环境恶劣 |

**Node 系已组织化**：`NeteaseCloudMusicApiEnhanced` 组织（16 仓库、1.4k+ stars、多人协作）下有
`api-enhanced`（主项目）、`NCMAutoDaily`（cron 自动签到工程实例）、`api-clawer`（抓包工具，跟进新接口用）、
`api-framework`（通用 API 反代框架）、`UnblockNeteaseMusic-utils`。另有多个 revival fork 自行维护：
`HuaBofeng/NeteaseCloudMusicApi`（2026-08 活跃）、`wangzaifan/api-enhanced`（2026-07 活跃，**weapi 已支持
易盾 checkToken**）、`kexoub/muapi`（NeteaseCloudMusicApiReborn）。

结论：**原版已死、fork 求生且组织化**。风控是持续对抗，2025-2026 网易明显加严
（chaunsin 2025-06-03 公告"风控极为严格，刷歌有封号风险"）。

---

## 一·五、防风控技术纵深（"更牛"的方案在哪一层）

| 层 | 技术 | 状态 |
|---|---|---|
| 第一层 HTTP 伪装 | UA 轮换 / 随机 IP 头 / 限速退避 | **libnetease 已实现**（本次加固） |
| 第二层 协议反作弊 | 易盾 checkToken（已扩到 weapi，2026-07）、NMTID 服务端下发（2026-08）、xeapi 动态密钥 | api-enhanced 生态进行中；libnetease 2026-09 起已复用 jar 中服务端下发的 NMTID |
| 第三层 TLS 指纹 | JA3/JA4 伪装：标准 libcurl 的 TLS Client Hello 与浏览器差异巨大，防 bot 平台按此识别 curl | **libnetease 未做**（见 §七 TLS 指纹指南） |
| 第四层 身份与网络 | 扫码/Cookie 借真信任；住宅/移动代理池（数据中心 IP 在 TLS 之外被单独识别） | 全体上游共识 / 网络基建 |

### 各层具体方案的取舍（2026-10-10 评估，结论是「第二 / 三层的三条都不跟」）

用户要求「内核层完善好」，这三条是被点名的候选。逐条读实现后的结论是**都不做**，
理由不是「难」，而是**收益与可验证性不成比例**——记录下来免得下次重新论证：

- **易盾 checkToken**（api-enhanced 2026-07 已把它扩到 weapi）：要逆向网易前端混淆产物，
  签名随时可能变；更关键的是**我们拿不到「对/错」的可观测信号** —— 错了只表现为偶发
  `-460`，与 IP 脏、频率高、NMTID 缺失的症状完全重合，等于给自己加一段不能证伪的代码。
  真要查 `-460`，先看的顺序是：NMTID 是不是服务端真值 → 出口 IP → 频率。
- **xeapi**：读 `util/xeapiKey.js` 就清楚了 —— 它不是「会话密钥协商」，而是调
  `module/register_xeapikey` **向服务端注册并取回公钥 + sk**，还要跟 `currentKeyVersion`
  轮换。也就是说要跟进它，得**新增一条注册类接口并长期跟随 key 版本**，而收益只在高音质档位。
- **TLS 指纹 JA3/JA4**：**Android 侧本来就没这个问题** —— OkHttp 在 Android 上走
  Conscrypt(BoringSSL)，Client Hello 与 Chrome 的差距远小于标准 libcurl；我们唯一的 curl
  路径是桌面 CLI。为桌面换 curl-impersonate 要动构建与 CA 交互，风险大于收益。
  （§七 的指南保持有效，等真遇到 TLS 层拦截再上。）

**同轮真正补上的是「响应解码健壮性」**（与风控无关，见 USAGE.md §7.1）：压缩探测解压 +
按长度复制。它修的是「静默错值」——压缩体不解压会让服务端的 401 被读成 200。

---

## 二、风控信号速查表（先分类，再决策）

| 信号 | 含义 | 典型应对 |
|---|---|---|
| `HTTP 200` + 空 body（0 字节） | **数据中心/海外 IP 被风控丢弃**（最隐蔽；本项目开发沙箱的出口 IP 即此症状，已实证） | 换住宅 IP / 代理；`X-Real-IP`+`X-Forwarded-For` 国内 IP；`NE_RANDOM_CN_IP=1` |
| `code -460` / `460` "Cheating" | 签名校验失败 或 频率限制 / 疑似作弊 | 核对加密参数；减速；换 IP；退避重试 |
| `code -462` "网络太拥挤" | **行为验证**（滑块/验证码，常带 `verifyType`/`verifyUrl`），或 IP 太脏、缺登录 cookie | 引导浏览器登录 / 重新扫码；清理陈旧 cookie 重试（qr-key 已内置） |
| `code 8821` | 手机号密码登录需**行为验证码** | 换二维码登录 / Cookie 登录 |
| `HTTP 429` / `503` / `code 503` | 高频限流 / 服务过载 | 降频、加随机延迟、IP 代理池 |
| `HTTP 403` | WAF / 网关拦截 | 换 IP、补全 headers |
| `code 301` | 未登录 / 登录态过期（`MUSIC_U` 失效） | 重新登录 |
| `code 400` | 参数错误（含加密参数过期） | 检查参数 |
| `code 520` | libnetease 传输层错误约定 | 网络重试 |
| 登录成功后被要求二次验证 | 账号风控（异地/设备异常） | 停止高频自动化，人工验证 |

> 另见 api-enhanced `SPECIAL_STATUS_CODES = {201, 302, 400, 502, 800, 801, 802, 803}`：
> 二维码登录的 800/801/802/803 属正常轮询状态，不是错误。

---

## 三、风险维度拆解（网易在查什么）

1. **IP 维度**：数据中心 IP（阿里/腾讯云/Vercel 等）与海外 IP 直连最易触发 460/空 body；
   同一 IP 高频（登录接口尤其敏感，Binaryify 文档明确"登录接口不能调用太频繁"）。
2. **设备指纹维度**：`deviceId`（eapi header）、`sDeviceId`（chainId）、`os/osver/appver/versioncode/buildver`
   `resolution/mobilename/channel` 一致性；`NMTID` 真实性；UA 与 os 匹配。
3. **请求节奏维度**：秒级连发、无随机性、固定间隔，都是机器特征。
4. **签名维度**：`params`/`encSecKey`（weapi）、`eparams`（linuxapi）、`params`+md5 信封（eapi）必须与官方一致；
   参数过期也会 460。
5. **Cookie 完整性**：`MUSIC_U`（登录态）、`__csrf`、`_ntes_nuid`、`NMTID` 缺失或伪造会被拦。
6. **行为模式**：刷播放/签到/批量写操作最危险（chaunsin 明示封号风险，见"非法挂机行为警告" issue #34）。

---

## 四、高风险操作清单

| 操作 | 风险等级 | 说明 |
|---|---|---|
| 高频调用登录接口（qr-check 轮询、login-cellphone/login-email） | 🔴 高 | 最易触发 503/IP 高频/462/8821 |
| 刷歌/刷播放量、每日任务自动化 | 🔴 极高 | **封号级**（chaunsin 2025-06 公告 + 非法挂机警告） |
| 签到/领奖励自动化 | 🟠 中高 | 自动领取奖励有封号风险（chaunsin 默认关闭该选项） |
| 批量写操作（like/subscribe/track-add/playlist 增删改） | 🟠 中高 | 需要稳定登录态 + 低频率 + 幂等设计 |
| 更换 IP/设备/UA 频繁横跳 | 🟡 中 | 身份不一致反而暴露 |
| 只读操作（search/playlist/detail/lyric/song-url） | 🟢 低 | 主流场景，注意频率即可 |
| 二维码登录 | 🟢 最低 | **当前最稳的登录方式**（上游共识） |

---

## 五、应对策略（分层，全部可选、默认关闭、不破坏 drop-in 兼容）

### 5.1 网络层（解决"我是谁"）
- **住宅 IP / 大陆服务器**：根治方案。数据中心 IP 直连 = 空 body（本项目开发沙箱的出口 IP 已实证）。
- **国内 IP 伪装头**（活跃上游共识，Binaryify 文档原话："增加 X-Real-IP: 任意国内 IP 即可解决"）：
  显式 `NE_REAL_IP=211.161.244.70`，或 `NE_RANDOM_CN_IP=1` 每次自动生成国内 IP
  （api-enhanced 的 `randomCNIP`、Meting-API 的 `cnip()` 同款，均同时注入 `X-Real-IP` + `X-Forwarded-For`）。
- **代理**：`NE_UNM_PROXY`（已有，Go 版对应 UNMProxyURL）+ `NE_NO_KEEPALIVE=1`（关连接复用，换 IP 立即生效）。
- **代理池轮换**：高频场景需要，单条腿跑不动。

### 5.2 请求层（解决"节奏像不像人"）
- **限速 + 抖动**：`NE_RATE_LIMIT_MS=800 NE_RATE_LIMIT_JITTER_MS=400`（每请求前随机睡 0.8~1.2s）。
- **退避重试**：`NE_RETRY_RISK=3`，对 -460/高频/传输错误按 200ms×2ⁿ+抖动 重试；
  **-462 行为验证不在自动重试集合**（重试无意义，需人工）；写操作建议关闭或自行幂等。
- **UA 轮换**：`NE_UA_ROTATE=1`，PC 浏览器 UA 池（Chrome/Edge/Firefox/Safari）轮换。
- **缓存读接口**：Binaryify 靠 2 分钟缓存防"IP 高频"，宿主层自行实现。

### 5.3 身份层（解决"指纹一致性"）
- **登录态持久化**：cookie jar 已落盘 `~/.cache/netune/cookies.txt`，跨进程复用 `MUSIC_U`（已有）。
- **设备 ID 持久化**：`sDeviceId` 由 jar 持久化（已有）；eapi `deviceId` 走 24641 设备池（已有，
  数据与 api-enhanced `data/deviceid.txt` 同源）。
- **优先二维码登录**；手机号/邮箱密码登录尽量不用（8821/行为验证码频发）。
- **NMTID 真实性**（上游最新教训，见 §6）。

### 5.4 错误处理层（解决"挂了怎么办"）
- 用 `ne_risk_classify()` 分类响应，`ne_risk_reason()` 给出人话，`ne_risk_is_transient()` 判断可否自动重试。
- qr-key 已内置 -462/失败时清 cookie 重建会话重试。
- 空 body → 提示换 IP/代理（比"解析失败"有用得多）。

### 5.5 账号行为层（解决"别把号作死"）
- 不刷歌、不批量签到；写操作低频 + 幂等。
- 登录后不要高频重复登录（Binaryify：登录状态存在就别重复调登录接口）。

---

## 六、活跃上游最新实践（2025-2026）与 libnetease 对照

| 实践 | 上游实现 | libnetease 现状 | 处理 |
|---|---|---|---|
| 国内 IP 双头注入 | api-enhanced/Meting：`X-Real-IP`+`X-Forwarded-For` | 原无；本次新增 | ✅ 已实现（`NE_REAL_IP`/`NE_RANDOM_CN_IP`） |
| 随机中国 IP 生成 | api-enhanced 4147 CIDR 权重随机；Meting 内置城市段 | 原无 | ✅ 已实现（`ne_random_cn_ip()`） |
| NMTID 由服务端下发 | api-enhanced 2026-08-24 修复：不带 NMTID 请求 eapi 接口，从 Set-Cookie 采集真 NMTID，保底 `00O`+随机；**固定假值会触发风控** | Go v1.6.0 行为：固定 `some_random_id_from_strategy`（os=pc 策略） | ✅ 2026-09 起：Strategy 仅在 jar 无 NMTID/空值时才注入假值（且 filterJar 不上盘），jar 中服务端下发的真值优先复用，对齐 api-enhanced。**未移植**其"主动 eapi 探测采集"（缺省仍为假值），如需可参考上游 |
| 易盾反作弊 token（注册/验证码接口） | api-enhanced：jsdom 跑 Watchman SDK 取 `X-antiCheatToken`（v2/v3） | 无注册/验证码功能 | 📋 文档记录，如需扩展注册功能再实现 |
| 游客匿名 token（MUSIC_A） | api-enhanced/Meting：未登录注入游客 `MUSIC_A` | 无 | 📋 文档记录 |
| xeapi 第三套加密（2024+ 新增） | api-enhanced 已支持（xeapiKey 独立文件） | 无 | 📋 文档记录，libnetease 当前接口用不到 |
| -460 自动重试 | Meting：重试 5 次、间隔 100ms | 原无 | ✅ 已实现（`NE_RETRY_RISK`，指数退避） |
| 请求限速+抖动 | 爬虫社区通用 | 原无 | ✅ 已实现 |
| UA 轮换 | Meting：移动/PC 双池随机 | 原固定 UA | ✅ 已实现（`NE_UA_ROTATE`，PC 池） |
| cookie 指纹字段（_ntes_nnid/WNMCID/WEVNSM） | api-enhanced 注入完整 web 指纹 | 仅 _ntes_nuid（登录 URL 注入随机 NMTID） | 📋 文档记录，web 登录场景可参考 |

---

## 七、libnetease 加固清单（本次新增，全部 opt-in）

### 新 API
| API | 说明 |
|---|---|
| `ne_risk_classify(const ne_resp*)` | 响应 → 风控分类（8 类） |
| `ne_risk_reason(ne_risk_class)` | 人话原因 |
| `ne_risk_is_transient(ne_risk_class)` | 是否可自动退避重试 |
| `ne_risk_backoff_ms(attempt, max_ms)` | 指数退避 + 抖动 |
| `ne_risk_is_empty_body(const ne_resp*)` | 空 body（数据中心 IP 风控）检测 |
| `ne_random_cn_ip(char[16])` | 随机国内 IPv4 |
| `ne_http_set_real_ip(const char*)` | X-Real-IP + X-Forwarded-For |
| `ne_http_set_rate_limit(base_ms, jitter_ms)` | 请求间隔限速 |
| `ne_http_set_no_keepalive(int)` | 关闭连接复用 |
| `ne_set_risk_retry(int)` | 风控/传输错误自动重试 |
| `ne_sleep_ms(int64_t)` | 毫秒睡眠（util） |

### 新环境变量
| 变量 | 默认 | 说明 |
|---|---|---|
| `NE_REAL_IP` | 关 | 注入 `X-Real-IP`+`X-Forwarded-For`（国内 IP 规避 460/空 body） |
| `NE_RANDOM_CN_IP` | 关 | 未设 NE_REAL_IP 时每请求自动生成国内 IP |
| `NE_RATE_LIMIT_MS` / `NE_RATE_LIMIT_JITTER_MS` | 0 / 0 | 请求前随机间隔 base+[0,jitter) ms |
| `NE_UA_ROTATE` | 关 | PC 浏览器 UA 池轮换 |
| `NE_RETRY_RISK` | 0 | 对 -460/高频/传输错误指数退避重试 n 次 |
| `NE_NO_KEEPALIVE` | 关 | 关连接复用（配合代理换 IP） |
| `NE_BROWSER_HEADERS` | 关 | 补浏览器标准头（Accept/Accept-Language/Sec-Fetch 系/Upgrade-Insecure-Requests） |
| `NE_HTTP2` | 关 | 显式协商 HTTP/2（浏览器标配；需 libcurl 带 nghttp2，stub 测试勿开） |

### 行为变更
- 默认零变更：所有新功能关闭时，请求字节流与 Go v1.6.0 移植完全一致（dualrun 兼容不受影响）。
- `ne_resp` 增加 `long http_status` 字段（兼容扩展，不破坏既有使用）。

### 推荐组合（以低风险读接口为例）
```sh
export NE_RATE_LIMIT_MS=800 NE_RATE_LIMIT_JITTER_MS=400   # 真人节奏
export NE_RETRY_RISK=3                                     # -460/高频自愈
export NE_UA_ROTATE=1                                      # UA 分散
export NE_RANDOM_CN_IP=1                                   # 国内 IP 伪装（数据中心出口必开）
export NE_BROWSER_HEADERS=1                                # 请求头浏览器化
# 海外服务器建议再配住宅/优质代理 + NE_NO_KEEPALIVE=1
# 进阶（见下节 TLS 指纹）：Linux 下 LD_PRELOAD=libcurl-impersonate.so
```

---

## 八、TLS 指纹（第三层防风控）接入指南

> 现状：libnetease 的默认 transport 是**标准 libcurl**，其 TLS `Client Hello` 与真实浏览器差异
> 巨大（JA3/JA4 指纹明显）——若网易启用 TLS 指纹检测（防 bot 平台普遍做法，未公开证实），
> HTTP 层伪装得再好也会在 TLS 层暴露。本指南提供两种接入路径，均不改一行业务代码。

### 路径 A（Linux，零代码）：LD_PRELOAD 注入 curl-impersonate 的 libcurl
```sh
# 1. 安装 curl-impersonate（提供 libcurl-impersonate.so，BoringSSL/NSS 编译）
#    Ubuntu/Debian: apt install curl-impersonate-chrome（或从 lwthiker/curl-impersonate 构建）
# 2. 运行 netease-cli / 宿主程序时注入：
LD_PRELOAD=/path/to/libcurl-impersonate.so CURL_IMPERSONATE=chrome116 \
  netease-cli song-url 123
```
原理：`LD_PRELOAD` 让进程内所有 libcurl 句柄自动获得浏览器级 TLS/HTTP2 指纹，
`curl_easy_impersonate()` 由注入库自动调用，libnetease 无需感知。

### 路径 B（代码）：替换 transport
用 `ne_http_set_transport()` 注入一个基于 curl-impersonate（或 OkHttp/浏览器内核）的
transport——参考 `src/core/http.c` 中 `ne_http_transport` 结构，实现 `request` 回调即可。

### 配套（本库已内置）
- `NE_HTTP2=1`：显式协商 HTTP/2（浏览器标配，HTTP/2 SETTINGS/头顺序也是指纹来源之一）；
- `NE_BROWSER_HEADERS=1`：补齐浏览器标准头，缩小 HTTP 层差异；
- `NE_UA_ROTATE=1` + `NE_RANDOM_CN_IP=1`：HTTP 层基础伪装（第一层）。

> 注意：网易云是否启用 TLS 指纹检测**未公开证实**，以上属前瞻性加固。且 TLS 指纹只是
> 第三层，第四层（住宅 IP/借真信任）才是决定上限的因素——四层要配套，单点加固效果有限。

---

## 九、合规与账号安全提醒

- 本项目与所有上游一样**仅供个人学习使用**；请勿用于商业用途、刷量、版权规避。
- 逆向 API 违反网易服务条款；账号封禁风险自负（chaunsin 已明示"收到非法挂机行为警告请立即终止"）。
- 不要自动化高风险操作（刷歌/签到/批量写）；优先二维码登录；`MUSIC_U` 属于账号敏感凭证，妥善保管。
