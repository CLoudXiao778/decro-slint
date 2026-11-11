#ifndef NE_VENDOR_INFLATE_H
#define NE_VENDOR_INFLATE_H
#include <stddef.h>
#include <stdint.h>

/* 自带 DEFLATE 解压（RFC 1951）+ zlib / gzip 包装（RFC 1950 / 1952）。
 *
 * 为什么这个库需要它：传输层是**可注入**的（Android 宿主用 OkHttp，
 * 见 USAGE.md §3），而各家的自动解压覆盖不齐 —— OkHttp 只解
 * `Content-Encoding: gzip`，碰到 `deflate`（zlib 或 raw）会把压缩体
 * 原样交回来；curl 只解它自己在 Accept-Encoding 里宣告过的编码，且
 * 服务端若**不带** Content-Encoding 直接发 zlib 体则谁都不解。
 * 上游 Go 版正是在响应处手动补了这一手（request.go「数据被压缩 进行解码」，
 * 用 zlib.NewReader 试探性解压）。这里补上等价能力。
 *
 * 与 vendor/ 里的 AES / MD5 / RSA 同一套哲学：零外部依赖，桌面与
 * Android 逐字节同行为，不引入 `find_package(ZLIB)` 这种构建面差异。
 *
 * **失败是安全的**：任何探测失败 / 结构非法 / 超上限都返回 NULL，调用方
 * 原样使用输入。所以本模块的 bug 只会退化成「不解压」，不会损坏数据。
 *
 * **刻意不校验尾部校验和**（zlib 的 adler32 / gzip 的 CRC32+ISIZE）：调用的
 * 目的是「把服务端发来的压缩体读成 JSON」，随后 JSON 解析本身就是完备的
 * 完整性判据 —— 数据坏了 JSON 一定解析不出来。多算一遍 CRC 只是给每个响应
 * 白加一次 O(n) 遍历。反过来这也意味着：**尾部字节被篡改时本模块察觉不到**，
 * 这不是缺陷，是取舍（tests/test_inflate.c 的损坏用例只篡改数据区中部）。 */

/* 上限保护（防解压炸弹）：max_out = 0 时用内部默认 64 MiB。 */
#define NE_INFLATE_DEFAULT_MAX (64u * 1024u * 1024u)

/* 探测包装并解压。gzip（1f 8b）与 zlib（78 ..）自动识别；
 * raw deflate 无任何头部，无法与普通数据区分，必须走 [ne_inflate_raw]。
 * 成功返回 malloc 缓冲（长度为 *out_len，**不补 NUL**），失败返回 NULL。 */
uint8_t *ne_inflate(const uint8_t *in, size_t in_len, size_t *out_len, size_t max_out);

/* 按 raw deflate 解压（跳过包装探测）。 */
uint8_t *ne_inflate_raw(const uint8_t *in, size_t in_len, size_t *out_len, size_t max_out);

/* 头部探测：给调用方拼判据用，不分配、不失败。 */
int ne_inflate_has_gzip_header(const uint8_t *in, size_t n);
int ne_inflate_has_zlib_header(const uint8_t *in, size_t n);
#endif
