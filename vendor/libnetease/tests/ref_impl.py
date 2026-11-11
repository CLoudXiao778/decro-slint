#!/usr/bin/env python3
"""Reference implementation mirroring netease-music v1.6.0 util/cryto.go.

Emits tests/expected.h with deterministic vectors (fixed secret keys) so the
C port can be compared BYTE-FOR-BYTE against the Go behaviour.

Go-isms reproduced here on purpose:
  - json.Marshal(map) sorts keys and HTML-escapes <, >, & (\u003c ...).
  - rsaEncrypt: 16-byte key zero-padded LEFT to 128 bytes, c^65537 mod n,
    result serialized via big.Int.Bytes() (leading zero bytes stripped).
  - NewLen16Rand's "reverse" key is an independent random (test injects both).
"""
import base64
import hashlib
import json
import sys

from Crypto.Cipher import AES

IV = b"0102030405060708"
PRESET = b"0CoJUm6Qyw8W8jud"
EAPI_KEY = b"e82ckenh8dichen8"

PEM_B64 = ("MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDgtQn2JZ34ZC28NWYpAUd98iZ3"
           "7BUrX/aKzmFbt7clFSs6sXqHauqKWqdtLkF2KexO40H1YTX8z2lSgBBOAxLsvakl"
           "V8k4cBFK9snQXE9/DDaFt6Rr7iVZMldczhC0JNgTz+SHXT6CBHuX3e9SdB1Ua44o"
           "ncaTWz7OBGLbCiK45wIDAQAB")


# ---------- Go json.Marshal semantics ----------
def go_escape_string(s: str) -> str:
    out = []
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif ch == "\t":
            out.append("\\t")
        elif ch in "<>&":
            out.append("\\u%04x" % o)
        elif o < 0x20:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    return '"' + "".join(out) + '"'


def go_marshal(obj) -> str:
    """json.Marshal for dict-of-str / dict-of-dict (nested one level used by eapi)."""
    if not isinstance(obj, dict):
        raise TypeError("only maps supported")
    parts = []
    for k in sorted(obj.keys()):
        v = obj[k]
        if isinstance(v, dict):
            val = go_marshal(v)
        else:
            val = go_escape_string(str(v))
        parts.append(go_escape_string(str(k)) + ":" + val)
    return "{" + ",".join(parts) + "}"


# ---------- crypto primitives ----------
def pkcs7(data: bytes) -> bytes:
    n = 16 - (len(data) % 16)
    return data + bytes([n]) * n


def aes_cbc(data: bytes, key: bytes, iv: bytes) -> bytes:
    return AES.new(key, AES.MODE_CBC, iv).encrypt(pkcs7(data))


def aes_ecb(data: bytes, key: bytes) -> bytes:
    return AES.new(key, AES.MODE_ECB).encrypt(pkcs7(data))


def der_walk_pubkey() -> tuple[int, int]:
    der = base64.b64decode(PEM_B64)
    pos = 0

    def rd(b, p):
        tag = b[p]
        p += 1
        ln = b[p]
        p += 1
        if ln & 0x80:
            cnt = ln & 0x7F
            ln = int.from_bytes(b[p:p + cnt], "big")
            p += cnt
        return tag, ln, p

    tag, ln, pos = rd(der, pos)          # SEQ SubjectPublicKeyInfo
    tag, ln2, pos = rd(der, pos)         # SEQ AlgorithmIdentifier
    pos += ln2
    tag, ln3, pos = rd(der, pos)         # BITSTRING
    pos += 1                             # unused-bits byte
    tag, ln4, pos = rd(der, pos)         # SEQ RSAPublicKey
    tag, ln5, pos = rd(der, pos)         # INTEGER modulus
    n = int.from_bytes(der[pos:pos + ln5], "big")
    pos += ln5
    tag, ln6, pos = rd(der, pos)         # INTEGER exponent
    e = int.from_bytes(der[pos:pos + ln6], "big")
    return n, e


N, E = der_walk_pubkey()


def rsa_encrypt_secretkey(secret: bytes) -> bytes:
    buf = bytes(112) + secret            # zero left-pad to 128
    c = int.from_bytes(buf, "big")
    r = pow(c, E, N)
    return r.to_bytes(128, "big").lstrip(b"\x00")   # big.Int.Bytes()


def weapi_det(data: dict, secret: bytes, resecret: bytes) -> dict:
    text = go_marshal(data)
    c1 = aes_cbc(text.encode(), PRESET, IV)
    b1 = base64.b64encode(c1)
    c2 = aes_cbc(b1, resecret, IV)
    return {
        "params": base64.b64encode(c2).decode(),
        "encSecKey": rsa_encrypt_secretkey(secret).hex(),
    }


def eapi(url: str, data: dict) -> str:
    text = go_marshal(data)
    message = "nobody" + url + "use" + text + "md5forencrypt"
    digest = hashlib.md5(message.encode()).hexdigest()
    dd = url + "-36cd479b6b5-" + text + "-36cd479b6b5-" + digest
    return aes_ecb(dd.encode(), EAPI_KEY).hex().upper()


# ---------- vector generation ----------
def main():
    cases = []

    # md5
    cases.append(("MD5_EMPTY", hashlib.md5(b"").hexdigest()))
    cases.append(("MD5_ABC", hashlib.md5(b"abc").hexdigest()))

    # AES-128 single block (FIPS-197 C.1)
    nist_key = bytes(range(16))
    nist_pt = bytes.fromhex("00112233445566778899aabbccddeeff")
    ct = AES.new(nist_key, AES.MODE_ECB).encrypt(nist_pt).hex()
    cases.append(("AES_NIST", ct))

    # jmap marshal with Go escaping
    cases.append(("JMAP1", go_marshal({"a": "<&>", "b": "line\nnext", "c": "plain"})))

    # RSA modulus (checks the DER parse)
    cases.append(("RSA_N_HEX", format(N, "x")))

    # weapi — realistic payloads, fixed keys
    w1 = weapi_det(
        {"csrf_token": "", "ids": "[347230,347231]", "hello": "<&>world"},
        b"A1b2C3d4E5f6G7h8", b"Z9y8X7w6V5u4T3s2")
    cases.append(("WEAPI1_PARAMS", w1["params"]))
    cases.append(("WEAPI1_ENC", w1["encSecKey"]))

    w2 = weapi_det({"csrf_token": ""}, b"0123456789abcdef", b"fedcba9876543210")
    cases.append(("WEAPI2_PARAMS", w2["params"]))
    cases.append(("WEAPI2_ENC", w2["encSecKey"]))

    # eapi — with nested header like request.go builds
    e1 = eapi("/api/song/enhance/player/url", {
        "ids": "[347230]",
        "br": "128000",
        "header": {"os": "pc", "appver": "9.0.65"},
    })
    cases.append(("EAPI1_PARAMS", e1))

    print("/* auto-generated by tests/ref_impl.py — do not edit */")
    print("#ifndef EXPECTED_H")
    print("#define EXPECTED_H")
    for name, val in cases:
        cval = val.replace("\\", "\\\\").replace('"', '\\"')
        print(f'static const char *EXP_{name} = "{cval}";')
    print("#endif")


if __name__ == "__main__":
    main()
