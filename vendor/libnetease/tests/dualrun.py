#!/usr/bin/env python3
"""netease-cli differential test harness (phase 7).

Two modes:

  1. C-only (default): runs the C netease-cli against a stub music.163.com
     and checks stdout / stderr / exit code / cookie-jar state against
     expectations derived from the Go shell's semantics (main.go).

  2. Dual-run (--go /path/to/netease-cli): runs BOTH the Go binary and the
     C binary with identical argv, identical HOME (fresh per case, seeded
     cookies) and identical NE_API_BASE, then byte-compares rc, stdout and
     stderr. The only tolerated difference is the chainId millisecond
     timestamp inside qr-key output, which is normalized before the diff.

The stub server covers all three wire channels and validates them:
  - weapi: decrypts the outer AES-CBC(presetKey) layer of `params`
  - linuxapi (/api/linux/forward): fully decrypts `eparams` AES-ECB and
    dispatches on the inner url
  - eapi: fully decrypts `params` hex AES-ECB(eapiKey) and verifies the
    md5 envelope
Decryption validation needs pycryptodome; without it the stub still
dispatches (validation is skipped) so the harness runs anywhere.

Usage:
    python3 tests/dualrun.py --cli build/netease-cli
    python3 tests/dualrun.py --cli build/netease-cli --go ./netease-cli-go
"""
import argparse
import base64
import hashlib
import io
import json
import os
import re
import shutil
import socketserver
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs

HERE = os.path.dirname(os.path.abspath(__file__))
CERT_PATH = os.path.join(HERE, "fixtures", "necert.pem")
KEY_PATH = os.path.join(HERE, "fixtures", "nekey.pem")

try:
    from Crypto.Cipher import AES
    HAVE_CRYPTO = True
except ImportError:
    HAVE_CRYPTO = False

EAPI_KEY = b"e82ckenh8dichen8"      # cryto.go eapiKey
PRESET = b"0CoJUm6Qyw8W8jud"        # cryto.go presetKey
IV = b"0102030405060708"
LINUX_KEY = b"rFgB&h#%2?^eDg:Q"     # cryto.go linuxapiKey

# fixed device id so qr-key's chainId is deterministic modulo the timestamp
SDEVICE = "CAFE0000CAFE0000CAFE0000CAFE0000CAFE0000CAFE0000"

# ── canned responses (raw compact JSON — passthrough cmds print these bytes) ──
ACCOUNT = ('{"code":200,"account":{"id":12345,"userName":"u@x"},'
           '"profile":{"nickname":"测试用户","vipType":11}}')
LIKEIDS = '{"code":200,"ids":[111,222,333]}'
SONGS = ('{"code":200,"songs":['
         '{"id":111,"name":"歌&A","ar":[{"name":"张三"}]},'
         '{"id":222,"name":"歌B","ar":[{"name":"李四"}]},'
         '{"id":333,"name":"歌C","ar":[{"name":"王五"}]}]}')
PLAYLIST_DETAIL = ('{"code":200,"playlist":{"id":777,"name":"列表","tracks":'
                   '[{"id":111,"name":"歌&A"},{"id":222,"name":"歌B"}]}}')
USER_PLAYLIST = ('{"code":200,"playlist":['
                 '{"id":777,"name":"我的红心","subscribed":false},'
                 '{"id":888,"name":"收藏&分享","subscribed":true}]}')
SONGURL_V1 = ('{"code":200,"data":[{"code":200,"url":"http://a/v1.mp3",'
              '"freeTrialInfo":{"start":0,"end":30}}]}')
SONGURL_OLD = ('{"code":200,"data":[{"code":200,"url":"http://a/old.mp3",'
               '"br":320000}]}')
SONGURL_DL = ('{"code":200,"data":{"code":200,"url":"http://a/dl.mp3",'
              '"br":320000,"size":12345}}')
SONGURL_OK = ('{"code":200,"data":[{"code":200,"url":"http://a/v1.mp3",'
              '"br":320000,"level":"exhigh"}]}')
SONGURL_DENY = ('{"code":200,"data":[{"code":-460,"msg":"need vip"}]}')
VIP_INFO = ('{"code":200,"data":{"redVipLevel":5,"redVipAnnualCount":-1,'
            '"redVipExpireTime":1700000000000}}')
CHECKURL = '{"code":200,"data":[{"code":200,"url":"http://a/check.mp3"}]}'
RECENT = ('{"code":200,"data":{"list":[{"playCount":3,'
          '"song":{"id":111,"name":"歌&A"}}]}}')
LYRIC = ('{"code":200,"lrc":{"lyric":"[00:01]原文"},'
         '"tlyric":{"lyric":"[00:01]翻译"},"klyric":{"lyric":""}}')
TOPLIST = '{"code":200,"list":[{"id":1,"name":"飙升榜"}]}'
RECOMMEND_RES = '{"code":200,"recommend":[{"id":555,"name":"每日推荐"}]}'
RECOMMEND_SONGS = ('{"code":200,"data":{"dailySongs":'
                   '[{"id":111,"name":"歌&A"}]}}')
SEARCH = '{"code":200,"result":{"songs":[{"id":111,"name":"歌&A"}]}}'
QRKEY = '{"code":200,"unikey":"smoke_unikey_1"}'
QRCHECK_WAIT = '{"code":800,"message":"waiting"}'
QRCHECK_OK = ('{"code":803,"cookie":"MUSIC_U=tok3; Path=/; Max-Age=31536000",'
              '"data":{"code":803}}')
LIKE_WRITE = '{"code":200,"playlistId":999}'
OK = '{"code":200}'
TRACKS_OP = '{"code":200,"body":{"code":200}}'
CREATE = ('{"code":200,"id":999,"playlist":{"id":999,"name":"新列表"}}')
RENAME = '{"code":200,"playlistId":777}'
LOGIN_EMAIL = '{"code":200,"cookie":"MUSIC_U=tok"}'
LOGIN_CELL = '{"code":200,"cookie":"MUSIC_U=tok2"}'

LOG = sys.stderr


def unpad(b):
    return b[:-b[-1]]


def respond_raw(handler, text):
    body = text.encode("utf-8")
    handler.send_response(200)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)
    handler.wfile.flush()


def dispatch(state, conn, url, cookie):
    # linuxapi inner urls arrive as absolute http://host/api/... (request.c
    # forwards the full rewritten url); weapi/eapi arrive as bare /weapi|/eapi
    # paths. Collapse both to a pure path so the routes below compare exactly.
    if "://" in url:
        authority = url.split("://", 1)[1]
        url = "/" + authority.split("/", 1)[1] if "/" in authority else "/"
    url = url.split("?")[0].split("#")[0]
    # Exact-path routing only. Every path below is the precise wire path the
    # C client emits (weapi → /weapi/..., linuxapi inner url → /api/...,
    # eapi → /eapi/...). Substring matching previously let a typo'd path
    # (e.g. full/vip/info vs front/vip/info) silently pass; an unknown path
    # now falls through to 404 and fails the output diff.
    if url == "/eapi/playlist/update/name":
        return respond_raw(conn, RENAME)
    if url == "/weapi/cloudsearch/pc":
        return respond_raw(conn, SEARCH)
    if url == "/weapi/song/enhance/download/url/v1":
        return respond_raw(conn, SONGURL_DL)
    if url == "/weapi/song/enhance/player/url/v1":
        # check-quality probes a single level; the weapi params are
        # encrypted so the stub can't read the level — vary the verdict
        # via seeded marker cookies instead
        if "QDENY" in cookie:
            return respond_raw(conn, SONGURL_DENY)
        if "QOK" in cookie:
            return respond_raw(conn, SONGURL_OK)
        return respond_raw(conn, SONGURL_V1)
    if url == "/weapi/song/enhance/player/url":        # check-music + song-url old (both weapi since 2026-09)
        return respond_raw(conn, SONGURL_OLD)
    if url == "/weapi/song/lyric":                     # lyric (weapi since 2026-09; was linuxapi inner)
        return respond_raw(conn, LYRIC)
    if url == "/weapi/play-record/song/list":
        return respond_raw(conn, RECENT)
    if url == "/weapi/music-vip-membership/front/vip/info":
        return respond_raw(conn, VIP_INFO)
    if url == "/weapi/w/nuser/account/get":
        if "GARBAGE" in cookie:   # HTML instead of JSON
            body = b"<html>not json</html>"
            conn.send_response(200)
            conn.send_header("Content-Type", "text/html")
            conn.send_header("Content-Length", str(len(body)))
            conn.end_headers()
            conn.wfile.write(body)
            conn.wfile.flush()
            return
        return respond_raw(conn, ACCOUNT)
    if url == "/weapi/v3/song/detail":
        return respond_raw(conn, SONGS)
    if url == "/weapi/v6/playlist/detail":             # weapi (Go 版为 linuxapi inner /api/v3)
        return respond_raw(conn, PLAYLIST_DETAIL)
    if url == "/weapi/user/playlist":
        return respond_raw(conn, USER_PLAYLIST)
    if url == "/weapi/login/qrcode/unikey":
        return respond_raw(conn, QRKEY)
    if url == "/weapi/login/qrcode/client/login":
        # qr-check is double-encrypted weapi (stub can't read the
        # key) — dispatch on a marker cookie instead: seeded
        # QRDONE=1 → 803 confirmed with a server cookie string
        if "QRDONE" in cookie:
            return respond_raw(conn, QRCHECK_OK)
        return respond_raw(conn, QRCHECK_WAIT)
    if url == "/weapi/song/like/get":
        return respond_raw(conn, LIKEIDS)
    if url == "/weapi/song/like":
        return respond_raw(conn, LIKE_WRITE)
    if url in ("/weapi/playlist/subscribe", "/weapi/playlist/unsubscribe"):
        return respond_raw(conn, OK)
    if url == "/weapi/playlist/manipulate/tracks":
        return respond_raw(conn, TRACKS_OP)
    if url == "/weapi/playlist/create":
        return respond_raw(conn, CREATE)
    if url == "/weapi/playlist/remove":
        return respond_raw(conn, OK)
    if url in ("/api/login", "/api/login/", "/weapi/login", "/weapi/login/"):
        return respond_raw(conn, LOGIN_EMAIL)
    if url == "/weapi/w/login/cellphone":
        return respond_raw(conn, LOGIN_CELL)
    if url == "/weapi/login/token/refresh":
        return respond_raw(conn, OK)
    if url == "/weapi/toplist/detail":
        return respond_raw(conn, TOPLIST)
    if url in ("/weapi/v1/discovery/recommend/resource",
               "/weapi/personalized/playlist"):
        return respond_raw(conn, RECOMMEND_RES)
    if url == "/weapi/v3/discovery/recommend/songs":
        return respond_raw(conn, RECOMMEND_SONGS)
    return respond_raw(conn, '{"code":404,"message":"no stub ' + url + '"}')


def handle_post(state, conn, path, headers):
    """Shared POST pipeline used by both the plaintext C bridge and the
    Go TLS-CONNECT proxy. `conn` exposes rfile/wfile + response helpers."""
    n = int(headers.get("Content-Length", "0"))
    form = parse_qs(conn.rfile.read(n).decode("utf-8"))
    cookie = headers.get("Cookie", "")
    p = path
    print(f"  POST {p} cookie={cookie!r}", file=LOG)

    # ── linuxapi: decrypt eparams, dispatch on inner url ──
    if p.endswith("/linux/forward") and HAVE_CRYPTO:
        raw = bytes.fromhex(form["eparams"][0])
        inner = json.loads(
            unpad(AES.new(LINUX_KEY, AES.MODE_ECB).decrypt(raw))
            .decode("utf-8"))
        url = inner.get("url", "")
        print(f"    linuxapi inner: {json.dumps(inner, ensure_ascii=False)}",
              file=LOG)
        state["last_linuxapi"] = inner
        return dispatch(state, conn, url, cookie)

    # ── eapi: full decrypt + md5 envelope check ──
    if "/eapi/" in p:
        if HAVE_CRYPTO:
            raw = bytes.fromhex(form["params"][0])
            plain = unpad(
                AES.new(EAPI_KEY, AES.MODE_ECB).decrypt(raw)).decode()
            head, rest = plain.split("-36cd479b6b5-", 1)
            text, digest = rest.rsplit("-36cd479b6b5-", 1)
            ok = hashlib.md5(
                ("nobody" + head + "use" + text + "md5forencrypt")
                .encode()).hexdigest() == digest
            inner = json.loads(text)
            print(f"    eapi inner: url={head} md5_ok={ok} "
                  f"data={json.dumps(inner, ensure_ascii=False)}",
                  file=LOG)
            state["eapi_md5_ok"] = ok
            state["last_eapi"] = {"url": head, "data": inner}
        return dispatch(state, conn, p, cookie)

    # ── weapi: validate outer layer shape (log only) ──
    if "params" in form and HAVE_CRYPTO:
        try:
            layer1 = AES.new(PRESET, AES.MODE_CBC, IV).decrypt(
                base64.b64decode(form["params"][0]))
            print(f"    weapi layer1 ok ({len(layer1)}B)",
                  file=LOG)
        except Exception as e:  # noqa: BLE001
            print(f"    weapi layer1 FAILED: {e}", file=LOG)
            state["weapi_layer1_ok"] = False
    return dispatch(state, conn, p, cookie)


def make_handler(state):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            handle_post(state, self, self.path, self.headers)

    return H


class _Hdr:
    """Case-insensitive dict-like for raw header lines."""
    def __init__(self, raw):
        self.d = {k.lower(): v for k, v in raw}
    def get(self, key, default=None):
        return self.d.get(key.lower(), default)


class ProxyConn:
    """Minimal response-capable object used by the TLS-CONNECT proxy path."""
    def __init__(self, sock):
        self.sock = sock
        self.rfile = sock.makefile("rb")
        self.wfile = sock.makefile("wb")
    def send_response(self, code):
        self.wfile.write(("HTTP/1.1 %d OK\r\n" % code).encode())
    def send_header(self, k, v):
        self.wfile.write(("%s: %s\r\n" % (k, v)).encode())
    def end_headers(self):
        self.wfile.write(b"Connection: close\r\n\r\n")
        self.wfile.flush()


def _service_http(state, conn, first_line=None):
    """Serve one or more HTTP/1.1 requests on an already-established conn."""
    line = first_line
    for _ in range(100):
        if line is None:
            line = conn.rfile.readline()
            if not line:
                return
        try:
            method, target, _ver = line.decode("utf-8").strip().split(" ", 2)
        except ValueError:
            return
        # read header block
        headers = _Hdr([])
        while True:
            hl = conn.rfile.readline()
            if not hl or hl in (b"\r\n", b"\n"):
                break
            k, sep, v = hl.decode("latin-1").partition(":")
            headers.d[k.strip().lower()] = v.strip()
        # strip proxy-absolute-form authority if present
        path = target
        if target.startswith("http://") or target.startswith("https://"):
            path = "/" + target.split("/", 3)[-1] if "/" in target[8:] else "/"
        if method == "POST":
            handle_post(state, conn, path, headers)
        else:
            # GET (e.g. unused by the CLI) — 200 empty JSON
            respond_raw(conn, "{}")
        line = None


class TLSProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, state, server_address):
        self.state = state
        super().__init__(server_address, ProxyHandler)

    def get_request(self):
        sock, addr = super().get_request()
        sock.settimeout(30)
        return sock, addr


class ProxyHandler(socketserver.BaseRequestHandler):
    """Handles both CONNECT+TLS (https://music.163.com) and plaintext
    absolute-form requests (http://interface3.music.163.com eapi).

    A single persistent BufferedReader is kept for the whole connection so
    pipelined headers/body are never dropped when we switch modes."""

    def handle(self):
        sock = self.request
        try:
            buffered = sock.makefile("rb")
            first = buffered.readline()
            if not first:
                sock.close()
                return
            if first.upper().startswith(b"CONNECT "):
                # client won't send ClientHello until it sees our 200, so
                # nothing beyond the CONNECT line sits in `buffered`.
                sock.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                ctx.load_cert_chain(CERT_PATH, KEY_PATH)
                tls = ctx.wrap_socket(sock, server_side=True)
                _service_http(self.server.state, ProxyConn(tls))
                tls.close()
                sock.close()
                return
            # absolute-form plaintext (http://interface3.music.163.com/...):
            # `first` is already consumed, hand it back as the request line.
            conn = ProxyConn(sock)
            conn.rfile = buffered  # reuse the reader that holds the rest
            _service_http(self.server.state, conn, first_line=first)
            sock.close()
        except (ssl.SSLError, OSError, ValueError):
            try:
                sock.close()
            except OSError:
                pass


class Stub:
    def __init__(self):
        self.state = {"qr_check_count": 0, "weapi_layer1_ok": True,
                      "eapi_md5_ok": True}
        self.srv = HTTPServer(("127.0.0.1", 0), make_handler(self.state))
        self.port = self.srv.server_address[1]
        self.proxy_srv = TLSProxyServer(self.state, ("127.0.0.1", 0))
        self.proxy_port = self.proxy_srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        threading.Thread(target=self.proxy_srv.serve_forever,
                         daemon=True).start()

    @property
    def base(self):
        return f"http://127.0.0.1:{self.port}"

    @property
    def proxy(self):
        return f"http://127.0.0.1:{self.proxy_port}"


def seed_cookie_file(home, names_values, garbage=False):
    d = os.path.join(home, ".cache", "netune")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "cookies.txt")
    with open(path, "w", encoding="utf-8") as f:
        if garbage:
            f.write("music.163.com\tFALSE\t/\tFALSE\t253402300799\t"
                    "GARBAGE\tyes\n")
        for n, v in names_values:
            f.write(f"music.163.com\tFALSE\t/\tFALSE\t253402300799\t{n}\t{v}\n")
    return path


def run_cli(cli, argv, home, api_base, proxy=None):
    env = dict(os.environ)
    env["HOME"] = home
    env.pop("NE_UNM_PROXY", None)
    env.pop("NE_API_BASE", None)
    if proxy:
        # Go reference shell: route https/http via the stub TLS-CONNECT proxy
        env["NE_UNM_PROXY"] = proxy
    else:
        # C shell: hard URL-rewrite to the plaintext stub
        env["NE_API_BASE"] = api_base
    p = subprocess.run([cli] + argv, capture_output=True, env=env,
                       timeout=30)
    return p.returncode, p.stdout.decode("utf-8", "replace"), \
        p.stderr.decode("utf-8", "replace")


def read_cookies(home):
    path = os.path.join(home, ".cache", "netune", "cookies.txt")
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return ""


TS_NORM = re.compile(r"web_login_\d+")
HOME_NORM = re.compile(r"/tmp/dualrun(?:-go)?-[^/\s]+")


def norm(s):
    s = TS_NORM.sub("web_login_TS", s)
    # C and Go run in distinct mkdtemp homes (dualrun-* / dualrun-go-*);
    # the login-status output embeds the cookie path, so collapse the
    # varying per-run prefix to a stable placeholder before diffing.
    s = HOME_NORM.sub("/tmp/dualrun-HOME", s)
    return s


def load_qr_fixture():
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "fixtures", "qr_render_expected.txt"),
              "rb") as f:
        data = f.read()
    # normalize CRLF (Windows checkout) → LF so the byte compare passes
    # regardless of how the fixture file was saved
    return data.replace(b"\r\n", b"\n")


def png_ok(b64_text):
    """qr-image: base64 → PNG magic + IEND + sane size for 480px"""
    try:
        raw = base64.b64decode(b64_text.strip())
    except Exception:  # noqa: BLE001
        return False
    return (raw[:8] == b"\x89PNG\r\n\x1a\n" and raw.endswith(b"IEND\xaeB`\x82")
            and len(raw) > 500)


def build_cases(stub):
    """(name, argv, opts) — expectations from the Go shell's semantics"""
    C = []

    def case(name, argv, stdout=None, stdout_re=None, stderr="", rc=0,
             seed=True, seed_extra=None, garbage=False, cookie_has=None,
             check=None):
        C.append(dict(name=name, argv=argv, stdout=stdout, stdout_re=stdout_re,
                      stderr=stderr, rc=rc, seed=seed, seed_extra=seed_extra,
                      garbage=garbage, cookie_has=cookie_has, check=check))

    # ── offline commands ──
    case("login-status", ["login-status"],
         stdout_re=(r'^\{"status":"check [^"]*[\\/]\.cache[\\/]netune'
                    r'[\\/]cookies\.txt"\}\n$'),
         seed=False)
    case("qr-image", ["qr-image",
                      "https://music.163.com/login?codekey=abc123"],
         check=lambda out: png_ok(out), seed=False)
    case("qr-render", ["qr-render",
                       "https://music.163.com/login?codekey=abc123"],
         check=lambda out: out.replace("\r\n", "\n").encode("utf-8")
             == load_qr_fixture(),
         seed=False)

    # ── read family (passthrough: stub bytes + newline) ──
    for name, argv, body in [
        ("search", ["search", "abc"], SEARCH),
        ("search-pl", ["search-pl", "歌单"], SEARCH),
        ("song-detail", ["song-detail", "111,222"], SONGS),
        ("playlist", ["playlist", "777"], PLAYLIST_DETAIL),
        ("user-playlist", ["user-playlist", "12345"], USER_PLAYLIST),
        ("record-recent", ["record-recent"], RECENT),
        ("recommend-resource", ["recommend-resource"], RECOMMEND_RES),
        ("recommend-songs", ["recommend-songs"], RECOMMEND_SONGS),
        ("recommend-playlists", ["recommend-playlists"], RECOMMEND_RES),
        ("toplist", ["toplist"], TOPLIST),
    ]:
        case(name, argv, stdout=body + "\n")

    # ── derived-output commands (Go json.Marshal semantics) ──
    case("account-name", ["account-name"], stdout="测试用户\n")
    case("account-info", ["account-info"],
         stdout='{"login":true,"name":"测试用户","vipType":11}\n')
    case("vip-info", ["vip-info"], stdout=VIP_INFO + "\n")
    case("check-music", ["check-music", "111"],
         stdout='{"code":200,"playable":true}\n')
    case("liked-check hit", ["liked-check", "111"],
         stdout='{"code":200,"liked":true}\n')
    case("liked-check miss", ["liked-check", "999"],
         stdout='{"code":200,"liked":false}\n')
    case("liked", ["liked"],
         stdout='{"code":200,"result":{"songs":['
                '{"ar":[{"name":"张三"}],"id":111,"name":"歌\\u0026A"},'
                '{"ar":[{"name":"李四"}],"id":222,"name":"歌B"},'
                '{"ar":[{"name":"王五"}],"id":333,"name":"歌C"}]}}\n')
    case("playlists", ["playlists"],
         stdout='{"code":200,"playlists":['
                '{"coverImgUrl":"","id":777,"name":"我的红心","subscribed":false},'
                '{"coverImgUrl":"","id":888,"name":"收藏\\u0026分享","subscribed":true}]}\n')
    case("playlist-tracks", ["playlist-tracks", "777"],
         stdout='{"code":200,"result":{"songs":['
                '{"id":111,"name":"歌\\u0026A"},'
                '{"id":222,"name":"歌B"}]}}\n')
    case("lyric tlyric preferred", ["lyric", "111"],
         stdout='{"code":200,"lyric":"[00:01]翻译"}\n')
    case("song-url fallback", ["song-url", "111"],
         stdout='{"code":200,"data":'
                '[{"br":320000,"code":200,"url":"http://a/old.mp3"}]}\n')
    case("song-download-url", ["song-download-url", "111"],
         stdout='{"code":200,"data":[{"br":320000,"code":200,"size":12345,'
                '"url":"http://a/dl.mp3"}]}\n')
    case("song-download-url level", ["song-download-url", "111", "lossless"],
         stdout='{"code":200,"data":[{"br":320000,"code":200,"size":12345,'
                '"url":"http://a/dl.mp3"}]}\n')

    # ── quality entitlement (single-level probe, no fallback/ladder) ──
    case("song-url lossless ok", ["song-url", "111", "lossless"],
         stdout='{"code":200,"data":[{"br":320000,"code":200,'
                '"url":"http://a/old.mp3"}]}\n')
    case("song-url bad level", ["song-url", "111", "flac"],
         stderr="bad level: flac\n", rc=1)
    case("song-download-url bad level", ["song-download-url", "111", "flac"],
         stderr="bad level: flac\n", rc=1)
    case("check-quality granted", ["check-quality", "111", "exhigh"],
         stdout='{"code":200,"granted":true,"granted_br":320000,'
                '"granted_level":"exhigh","reason":"ok","requested":"exhigh"}\n',
         seed_extra=[("QOK", "1")])
    case("check-quality trial", ["check-quality", "111", "lossless"],
         stdout='{"code":200,"granted":false,"granted_br":0,'
                '"granted_level":"","reason":"free_trial","requested":"lossless"}\n')
    case("check-quality denied", ["check-quality", "111", "hires"],
         stdout='{"code":-460,"granted":false,"granted_br":0,'
                '"granted_level":"","reason":"denied","requested":"hires"}\n',
         seed_extra=[("QDENY", "1")])
    case("check-quality bad level", ["check-quality", "111", "flac"],
         stderr="bad level: flac\n", rc=1)

    # ── qr family ──
    case("qr-key", ["qr-key"],
         stdout_re=(r'^\{"unikey":"smoke_unikey_1","url":'
                    r'"http://music\.163\.com/login\?codekey=smoke_unikey_1'
                    r'&chainId=v1_' + SDEVICE + r'_web_login_\d+"\}\n$'))
    case("qr-check wait", ["qr-check", "smoke_unikey_1"],
         stdout='{"code":800,"body":' + QRCHECK_WAIT + '}\n')
    # 803 confirmed: server cookie string must be merged into the jar
    # (saveNeteaseCookies: name=value pairs, Path/Max-Age attrs dropped)
    case("qr-check 803 saves cookie", ["qr-check", "key_ok"],
         stdout='{"code":803,"body":' + QRCHECK_OK + '}\n',
         seed_extra=[("QRDONE", "1")],
         cookie_has=["MUSIC_U\ttok3"])

    # ── write family: {"code":%.0f,"body":<raw>} envelope ──
    case("like", ["like", "111", "true"],
         stdout='{"code":200,"body":' + LIKE_WRITE + '}\n')
    case("subscribe", ["subscribe", "777", "1"],
         stdout='{"code":200,"body":' + OK + '}\n')
    case("track-add", ["track-add", "777", "111"],
         stdout='{"code":200,"body":' + TRACKS_OP + '}\n')
    case("track-del", ["track-del", "777", "111"],
         stdout='{"code":200,"body":' + TRACKS_OP + '}\n')
    case("playlist-create", ["playlist-create", "新歌单"],
         stdout='{"code":200,"body":' + CREATE + '}\n')
    case("playlist-rename", ["playlist-rename", "777", "新名字"],
         stdout='{"code":200,"body":' + RENAME + '}\n')
    case("playlist-delete", ["playlist-delete", "777"],
         stdout='{"code":200,"body":' + OK + '}\n')

    # ── login family ──
    case("login-email", ["login-email", "a@b.c", "pw"],
         stdout=LOGIN_EMAIL + "\n")
    case("login-cellphone", ["login-cellphone", "13800000000", "pw"],
         stdout=LOGIN_CELL + "\n")
    case("login-refresh", ["login-refresh"], stdout=OK + "\n")

    # ── error paths: exit codes + stderr text ──
    case("no args", [], stderr="usage: netease-cli <cmd> [args...]\n", rc=1)
    case("unknown cmd", ["bogus"], stderr="unknown cmd: bogus\n", rc=1)
    case("song-url missing id", ["song-url"],
         stderr="usage: netease-cli song-url <id> [level]\n", rc=1)
    case("song-download-url missing id", ["song-download-url"],
         stderr="usage: netease-cli song-download-url <id> [level]\n", rc=1)
    case("qr-render missing url", ["qr-render"],
         stderr="usage: netease-cli qr-render <url>\n", rc=1)
    case("liked parse account failed", ["liked"], garbage=True,
         stderr=("parse account failed: invalid character '<' "
                 "looking for beginning of value\n"), rc=1)
    return C


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", required=True, help="C netease-cli binary")
    ap.add_argument("--go", default=None,
                    help="Go netease-cli binary for dual-run mode")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(args.cli):
        print(f"cli not found: {args.cli}", file=sys.stderr)
        return 2
    if not HAVE_CRYPTO:
        print("note: pycryptodome missing — envelope decryption not "
              "validated (dispatch still works)", file=sys.stderr)

    stub = Stub()
    print(f"stub music.163.com on {stub.base}", file=sys.stderr)
    cases = build_cases(stub)
    npass = nfail = 0
    failures = []

    for c in cases:
        home = tempfile.mkdtemp(prefix="dualrun-")
        try:
            pairs = [("sDeviceId", SDEVICE), ("MUSIC_U", "seedtoken")]
            if c["seed_extra"]:
                pairs += c["seed_extra"]
            if c["seed"] or c["garbage"] or c["seed_extra"]:
                seed_cookie_file(home, pairs, garbage=c["garbage"])
            rc, out, err = run_cli(args.cli, c["argv"], home, stub.base)
            ok, why = True, []
            if rc != c["rc"]:
                ok, _ = False, why.append(f"rc {rc} != {c['rc']}")
            if c["stdout"] is not None and out != c["stdout"]:
                ok = False
                why.append(f"stdout\n   got:  {out!r}\n   want: {c['stdout']!r}")
            if c["stdout_re"] is not None and not re.match(c["stdout_re"], out):
                ok = False
                why.append(f"stdout_re\n   got:  {out!r}\n   want: {c['stdout_re']!r}")
            if err != c["stderr"]:
                ok = False
                why.append(f"stderr\n   got:  {err!r}\n   want: {c['stderr']!r}")
            if c["check"] is not None and not c["check"](out):
                ok = False
                why.append("check(out) failed")
            if c["cookie_has"]:
                ck = read_cookies(home)
                for frag in c["cookie_has"]:
                    if frag not in ck:
                        ok = False
                        why.append(f"cookie missing {frag!r}")

            # ── dual-run: byte-compare against the Go binary ──
            if args.go:
                ghome = tempfile.mkdtemp(prefix="dualrun-go-")
                try:
                    if c["seed"] or c["garbage"] or c["seed_extra"]:
                        seed_cookie_file(ghome, pairs, garbage=c["garbage"])
                    grc, gout, gerr = run_cli(args.go, c["argv"], ghome,
                                              stub.base, proxy=stub.proxy)
                    if (grc, norm(gout), norm(gerr)) != (rc, norm(out), norm(err)):
                        ok = False
                        why.append(
                            f"dual-run mismatch vs Go\n"
                            f"   go: rc={grc} out={norm(gout)!r} err={norm(gerr)!r}\n"
                            f"   c:  rc={rc} out={norm(out)!r} err={norm(err)!r}")
                finally:
                    shutil.rmtree(ghome, ignore_errors=True)

            if ok:
                npass += 1
                print(f"PASS {c['name']}")
            else:
                nfail += 1
                failures.append(c["name"])
                print(f"FAIL {c['name']}")
                for w in why:
                    print(f"     {w}")
        finally:
            shutil.rmtree(home, ignore_errors=True)

    print(f"\n{npass} passed, {nfail} failed, {len(cases)} total"
          + (" (dual-run vs Go)" if args.go else " (C-only)"))
    if not stub.state.get("weapi_layer1_ok", True):
        print("NOTE: some weapi layer1 decryption failed", file=sys.stderr)
        nfail += 1
    return 1 if nfail else 0


if __name__ == "__main__":
    sys.exit(main())
