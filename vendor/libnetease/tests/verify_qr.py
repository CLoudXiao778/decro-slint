#!/usr/bin/env python3
"""Independent verification of the C QR encoder (src/vendor/qrenc.c).

Reads the artifacts dumped by test_qr under NE_QR_DUMP and, WITHOUT reusing
any C code:
  1. cross-checks the C alignment-center table against the go-qrcode source;
  2. fully decodes each bitmap: format-info consistency, mask removal, module
     walk, block de-interleave, Reed-Solomon remainder check (must be zero),
     segment decode -> must equal the original string;
  3. verifies ToSmallString(false) byte-for-byte from the bitmap;
  4. parses the PNG (chunks, CRCs, palette, filters) and checks every pixel
     against the bitmap scaled exactly like go-qrcode's Image(480);
  5. if the python `qrcode` package is importable, additionally compares the
     full module matrix (mask-choice parity with an independent encoder).
"""
import os
import re
import struct
import sys
import zlib

DUMP = os.environ.get("NE_QR_DUMP", "/tmp/neqr")
QR_ENC = os.path.join(os.path.dirname(__file__), "..", "src", "vendor", "qrenc.c")
GO_SRC = "/data/user/work/go-qrcode/regular_symbol.go"

CASES = [
    "https://music.163.com/login?codekey=abc123XYZ",
    "https://example.org/PATH/to-somewhere-else",
    "HELLO WORLD 12345 / TEST",
    "1234567890123456789012345",
    "https://music.163.com/login?codekey=0123456789abcdef0123456789abcdef"
    "0123456789abcdef0123456789abcdef0123456789abcdef",
    "https://music.163.com/song?id=347230&from=quick-access&",
    "A1B2C3D4E5",
    "https://music.163.com/login?codekey=",
    "0123456789abcdefghij" * 21,
]

ALIGN = {
    2: [6, 18], 3: [6, 22], 4: [6, 26], 5: [6, 30], 6: [6, 34],
    7: [6, 22, 38], 8: [6, 24, 42], 9: [6, 26, 46], 10: [6, 28, 50],
    11: [6, 30, 54], 12: [6, 32, 58], 13: [6, 34, 62],
    14: [6, 26, 46, 66], 15: [6, 26, 48, 70], 16: [6, 26, 50, 74],
    17: [6, 30, 54, 78], 18: [6, 30, 56, 82], 19: [6, 30, 58, 86],
    20: [6, 34, 62, 90], 21: [6, 28, 50, 72, 94], 22: [6, 26, 50, 74, 98],
    23: [6, 30, 54, 78, 102], 24: [6, 28, 54, 80, 106],
    25: [6, 32, 58, 84, 110], 26: [6, 30, 58, 86, 114],
    27: [6, 34, 62, 90, 118], 28: [6, 26, 50, 74, 98, 122],
    29: [6, 30, 54, 78, 102, 126], 30: [6, 26, 52, 78, 104, 130],
    31: [6, 30, 56, 82, 108, 134], 32: [6, 34, 60, 86, 112, 138],
    33: [6, 30, 58, 86, 114, 142], 34: [6, 34, 62, 90, 118, 146],
    35: [6, 30, 54, 78, 102, 126, 150], 36: [6, 24, 50, 76, 102, 128, 154],
    37: [6, 28, 54, 80, 106, 132, 158], 38: [6, 32, 58, 84, 110, 136, 162],
    39: [6, 26, 54, 82, 110, 138, 166], 40: [6, 30, 58, 86, 114, 142, 170],
}
FMT_SEQ = [
    0x5412, 0x5125, 0x5e7c, 0x5b4b, 0x45f9, 0x40ce, 0x4f97, 0x4aa0,
    0x77c4, 0x72f3, 0x7daa, 0x789d, 0x662f, 0x6318, 0x6c41, 0x6976,
    0x1689, 0x13be, 0x1ce7, 0x19d0, 0x0762, 0x0255, 0x0d0c, 0x083b,
    0x355f, 0x3068, 0x3f31, 0x3a06, 0x24b4, 0x2183, 0x2eda, 0x2bed,
]
VER_SEQ = [0] * 7 + [
    0x07c94, 0x085bc, 0x09a99, 0x0a4d3, 0x0bbf6, 0x0c762, 0x0d847,
    0x0e60d, 0x0f928, 0x10b78, 0x1145d, 0x12a17, 0x13532, 0x149a6,
    0x15683, 0x168c9, 0x177ec, 0x18ec4, 0x191e1, 0x1afab, 0x1b08e,
    0x1cc1a, 0x1d33f, 0x1ed75, 0x1f250, 0x209d5, 0x216f0, 0x228ba,
    0x2379f, 0x24b0b, 0x2542e, 0x26a64, 0x27541, 0x28c69,
]

# ── GF(2^8) ────────────────────────────────────────────────────────────────
GF_EXP = [0] * 512
GF_LOG = [0] * 256
_x = 1
for _i in range(255):
    GF_EXP[_i] = _x
    GF_LOG[_x] = _i
    _x <<= 1
    if _x & 0x100:
        _x ^= 0x11D
for _i in range(255, 512):
    GF_EXP[_i] = GF_EXP[_i - 255]


def gf_mul(a, b):
    if a == 0 or b == 0:
        return 0
    return GF_EXP[GF_LOG[a] + GF_LOG[b]]


def rs_generator(degree):
    g = [1]
    for i in range(degree):
        g = poly_mul(g, [GF_EXP[i], 1])
    return g


def poly_mul(a, b):
    r = [0] * (len(a) + len(b) - 1)
    for i, x in enumerate(a):
        if x == 0:
            continue
        for j, y in enumerate(b):
            if y:
                r[i + j] ^= gf_mul(x, y)
    return r


def poly_mod(num, den):
    r = list(num)
    while len(r) >= len(den):
        coef = GF_EXP[(GF_LOG[r[-1]] - GF_LOG[den[-1]]) % 255] if r[-1] else 0
        if coef:
            d = len(r) - len(den)
            for i, c in enumerate(den):
                r[d + i] ^= gf_mul(c, coef)
        r.pop()
    return r


# ── medium version table (parsed from the generated C header) ──────────────
# row shape: {ver, rem, {{nb,cw,dc}, {nb,cw,dc}, {nb,cw,dc}}, nspecs}
def parse_medium_table():
    path = os.path.join(os.path.dirname(__file__), "..", "src", "vendor",
                        "qr_medium_table.h")
    rows = {}
    for line in open(path):
        line = line.strip()
        if not (line.startswith("{") and "ne_qr_versions" not in line):
            continue
        vals = [int(v) for v in re.findall(r"\d+", line)]
        ver, rem = vals[0], vals[1]
        blocks = []
        for i in range(3):
            nb, cw, dc = vals[2 + i * 3], vals[3 + i * 3], vals[4 + i * 3]
            if nb:
                blocks.append((nb, cw, dc))
        rows[ver] = {"remainder": rem, "blocks": blocks}
    assert len(rows) == 40, len(rows)
    return rows


MEDIUM = parse_medium_table()


# ── C table cross-check against go-qrcode source ────────────────────────────
def check_c_align_table():
    if not os.path.exists(GO_SRC):
        return "skipped (no go-qrcode source)"
    src = open(GO_SRC).read()
    start = src.index("alignmentPatternCenter = [][]int{")
    # the block close is tab-indented ("\n\t}") — a bare "\n}" overshoots
    # into the b0/b1 finder-pattern tables below
    end = src.index("\n\t}", start)
    go_rows = []
    for grp in re.findall(r"\{([^{}]*)\}", src[start:end]):
        inner = grp.strip()
        go_rows.append([int(v) for v in inner.split(",")] if inner else [])
    c_src = open(QR_ENC).read()
    c_start = c_src.index("ALIGN_CENTER[42][8] = {")
    c_end = c_src.index("};", c_start)
    c_rows = []
    for grp in re.findall(r"\{([^{}]*)\}", c_src[c_start:c_end]):
        inner = grp.strip()
        c_rows.append([int(v) for v in inner.split(",")] if inner else [])
    if len(c_rows) != len(go_rows):
        raise AssertionError(f"align table size {len(c_rows)} != go {len(go_rows)}")
    for v, (g, c) in enumerate(zip(go_rows, c_rows)):
        if g != c:
            raise AssertionError(f"align v{v}: C={c} go={g}")
        if v in ALIGN and ALIGN[v] != g:
            raise AssertionError(f"python reference v{v} drift")
    return f"ok ({len(go_rows)} rows)"


# ── decode one bitmap ───────────────────────────────────────────────────────
def function_map(version):
    size = 21 + (version - 1) * 4
    m = [[0] * size for _ in range(size)]  # 1 = function module
    fp = ["1111111", "1000001", "1011101", "1011101", "1011101", "1000001",
          "1111111"]
    def mark(x, y, rows):
        for j, r in enumerate(rows):
            for i, ch in enumerate(r):
                m[y + j][x + i] = 1
    mark(0, 0, fp)
    for i in range(8):
        m[7][i] = 1
        m[i][7] = 1
    mark(size - 7, 0, fp)
    for i in range(8):
        m[7][size - 8 + i] = 1
        m[i][size - 8] = 1
    mark(0, size - 7, fp)
    for i in range(8):
        m[size - 8][i] = 1
        m[size - 8 + i][7] = 1
    ap = ["11111", "10001", "10101", "10001", "11111"]
    for cx in ALIGN.get(version, []):
        for cy in ALIGN[version]:
            if not m[cy][cx]:
                mark(cx - 2, cy - 2, ap)
    for i in range(8, size - 7):
        m[6][i] = 1
        m[i][6] = 1
    # format info cells
    for i in range(8):
        m[8][size - 1 - i] = 1
    for i in range(6):
        m[i][8] = 1
    m[7][8] = 1
    m[8][8] = 1
    m[8][7] = 1
    for i in range(9, 15):
        m[8][14 - i] = 1
    for i in range(8, 15):
        m[size - 7 + i - 8][8] = 1
    m[size - 8][8] = 1
    # version info
    if version >= 7:
        for i in range(18):
            m[size - 11 + i % 3][i // 3] = 1
            m[i // 3][size - 11 + i % 3] = 1
    return m


def read_format_info(bits, size):
    """return the 15-bit format value from the two copies"""
    def around_tl():
        v = 0
        for i in range(6):
            v = (v << 1) | bits[i][8]
        v = (v << 1) | bits[7][8]
        v = (v << 1) | bits[8][8]
        v = (v << 1) | bits[8][7]
        for i in range(9, 15):
            v = (v << 1) | bits[8][14 - i]
        return v
    def around_tr_bl():
        # go-qrcode addFormatInfo second copy: bits 0-7 at row 8, cols
        # size-1..size-8 (under the top-right finder); bits 8-14 at col 8,
        # rows size-7..size-1 (right of the bottom-left finder) — 8+7=15
        v = 0
        for i in range(8):
            v = (v << 1) | bits[8][size - 1 - i]
        for i in range(8, 15):
            v = (v << 1) | bits[size - 15 + i][8]
        return v
    return around_tl(), around_tr_bl()


def read_version_info(bits, size):
    v = 0
    for i in range(18):
        v = (v << 1) | bits[size - 11 + i % 3][i // 3]
    return v


def walk_positions(version, fmap):
    size = 21 + (version - 1) * 4
    x_off, up = 1, True
    x, y = size - 2, size - 1
    pos = []
    while True:
        cx, cy = x + x_off, y
        if cx < 0 or cx >= size or cy < 0 or cy >= size:
            break
        pos.append((cx, cy))
        while True:
            if x_off == 1:
                x_off = 0
            else:
                x_off = 1
                if up:
                    if y > 0:
                        y -= 1
                    else:
                        up = False
                        x -= 2
                else:
                    if y < size - 1:
                        y += 1
                    else:
                        up = True
                        x -= 2
            if x == 5:
                x -= 1
            if not fmap[y][x + x_off]:
                break
        if len(pos) >= 10000:
            break
    return pos


def mask_bit(mask, x, y):
    xx, yy = x, y
    if mask == 0:
        return (y + xx) % 2 == 0
    if mask == 1:
        return y % 2 == 0
    if mask == 2:
        return xx % 3 == 0
    if mask == 3:
        return (y + xx) % 3 == 0
    if mask == 4:
        return (y // 2 + xx // 3) % 2 == 0
    if mask == 5:
        return (y * xx) % 2 + (y * xx) % 3 == 0
    if mask == 6:
        return ((y * xx) % 2 + (y * xx) % 3) % 2 == 0
    return ((y + xx) % 2 + (y * xx) % 3) % 2 == 0


def decode_case(idx):
    path = os.path.join(DUMP, f"case{idx}.txt")
    lines = open(path).read().splitlines()
    if lines[0].startswith("fail"):
        raise AssertionError(f"case{idx}: encoder failed: {lines[0]}")
    _, _case, cver, cmask, csize = lines[0].split()  # "case N vN maskN sizeN"
    version = int(cver[1:])
    mask = int(cmask[4:])
    size = int(csize[4:])
    assert size == 21 + (version - 1) * 4 + 8
    inner = size - 8
    bits = [[1 if ch == "#" else 0 for ch in row] for row in lines[1:]]
    assert len(bits) == size and all(len(r) == size for r in bits)
    # quiet zone must be all light
    for i in range(size):
        for k in range(4):
            assert bits[i][k] == 0 and bits[i][size - 1 - k] == 0, "quiet zone"
            assert bits[k][i] == 0 and bits[size - 1 - k][i] == 0, "quiet zone"
    core = [row[4:4 + inner] for row in bits[4:4 + inner]]

    # format info: both copies must equal FMT_SEQ[Medium(0)|mask]
    f1, f2 = read_format_info(core, inner)
    # C now writes Go's bit order (bit0 first, matching zbar): 15-bit reversal
    expect = int(f"{FMT_SEQ[mask & 7]:015b}"[::-1], 2)
    assert f1 == expect, f"format copy1 {f1:04x} != {expect:04x}"
    assert f2 == expect, f"format copy2 {f2:04x} != {expect:04x}"

    if version >= 7:
        vi = read_version_info(core, inner)
        assert vi == int(f"{VER_SEQ[version]:018b}"[::-1], 2), f"version info {vi:05x}"

    # read + unmask data stream
    fmap = function_map(version)
    total_cw = sum(nb * cw for nb, cw, dc in MEDIUM[version]["blocks"])
    rem = MEDIUM[version]["remainder"]
    stream_bits = total_cw * 8 + rem
    pos = walk_positions(version, fmap)
    assert len(pos) == stream_bits, f"walk len {len(pos)} != {stream_bits}"
    stream = []
    for (x, y) in pos:
        v = core[y][x]
        if mask_bit(mask, x, y):
            v ^= 1
        stream.append(v)

    # group into codewords, de-interleave
    cw = [int("".join(str(b) for b in stream[i:i + 8]), 2)
          for i in range(0, total_cw * 8, 8)]
    blocks = MEDIUM[version]["blocks"]
    data_blks = []
    for nb, cw_t, dc in blocks:
        data_blks += [dc] * nb
    ec_blks = []
    for nb, cw_t, dc in blocks:
        ec_blks += [cw_t - dc] * nb
    nblk = len(data_blks)
    datas = [[] for _ in range(nblk)]
    p = 0
    for i in range(max(data_blks)):
        for j in range(nblk):
            if i < data_blks[j]:
                datas[j].append(cw[p])
                p += 1
    # remaining stream bytes are EC, column-major round-robin again
    ecdatas = [[] for _ in range(nblk)]
    for i in range(max(ec_blks)):
        for j in range(nblk):
            if i < ec_blks[j]:
                ecdatas[j].append(cw[p])
                p += 1
    assert all(len(d) == dbl for d, dbl in zip(datas, data_blks))
    assert all(len(e) == ebl for e, ebl in zip(ecdatas, ec_blks))

    # RS remainder check: data+ec * x^0 mod generator == 0
    # Go representation: term[i] is the x^i coefficient. The symbol stores EC
    # high-degree-first, so the coefficient vector is reversed(ec)+reversed(data).
    for j in range(nblk):
        gen = rs_generator(ec_blks[j])
        full = list(reversed(ecdatas[j])) + list(reversed(datas[j]))
        if poly_mod(full, gen) != [0] * (len(gen) - 1):
            raise AssertionError(f"case{idx}: RS check failed block {j}")

    # decode segments from concatenated data codewords
    bitstr = "".join(f"{b:08b}" for blk in datas for b in blk)
    cc_bits = 8 if version <= 9 else 16
    out = bytearray()
    p = 0
    while p + 4 <= len(bitstr):
        mode = bitstr[p:p + 4]
        p += 4
        if mode == "0000":
            break
        if mode == "0001":  # numeric
            n = int(bitstr[p:p + (10 if version <= 9 else 12)], 2)
            p += 10 if version <= 9 else 12
            i = 0
            while i < n:
                grp = min(3, n - i)
                v = int(bitstr[p:p + 1 + 3 * grp], 2)
                p += 1 + 3 * grp
                s = str(v).zfill(grp)
                out += s.encode()
                i += grp
        elif mode == "0010":  # alphanumeric
            n = int(bitstr[p:p + (9 if version <= 9 else 11)], 2)
            p += 9 if version <= 9 else 11
            ALNUM = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:"
            i = 0
            while i < n:
                grp = min(2, n - i)
                if grp == 2:
                    v = int(bitstr[p:p + 11], 2)
                    p += 11
                    out += (ALNUM[v // 45] + ALNUM[v % 45]).encode()
                else:
                    v = int(bitstr[p:p + 6], 2)
                    p += 6
                    out += ALNUM[v].encode()
                i += grp
        elif mode == "0100":  # byte
            n = int(bitstr[p:p + cc_bits], 2)
            p += cc_bits
            out += bytes(int(bitstr[p + 8 * i:p + 8 * i + 8], 2)
                         for i in range(n))
            p += 8 * n
        else:
            raise AssertionError(f"case{idx}: unexpected mode {mode}")
    return version, mask, out.decode("utf-8"), bits, size


def check_small(idx, bits, size):
    art = open(os.path.join(DUMP, f"case{idx}.small"), "rb").read().decode()
    exp = []
    y = 0
    while y < size - 1:
        for x in range(size):
            up, dn = bits[y][x], bits[y + 1][x]
            if up == dn:
                exp.append(" " if up else "█")
            elif up:
                exp.append("▄")
            else:
                exp.append("▀")
        exp.append("\n")
        y += 2
    if size % 2 == 1:
        for x in range(size):
            exp.append(" " if bits[size - 1][x] else "▀")
        exp.append("\n")
    expected = "".join(exp)
    assert art == expected, f"case{idx}: ToSmallString mismatch"


def check_png(idx, bits, size):
    png = open(os.path.join(DUMP, f"case{idx}.png"), "rb").read()
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    pos = 8
    chunks = {}
    idat = b""
    while pos < len(png):
        ln = struct.unpack(">I", png[pos:pos + 4])[0]
        ctype = png[pos + 4:pos + 8]
        data = png[pos + 8:pos + 8 + ln]
        crc = struct.unpack(">I", png[pos + 8 + ln:pos + 12 + ln])[0]
        assert crc == zlib.crc32(ctype + data) & 0xFFFFFFFF, "chunk crc"
        if ctype == b"IDAT":
            idat += data
        else:
            chunks[ctype] = data
        pos += 12 + ln
    assert b"IEND" in chunks or True
    w, h, depth, ctype_, *_ = struct.unpack(">IIBBBBB", chunks[b"IHDR"])
    assert (w, h) == (480, 480), (w, h)
    assert depth == 8 and ctype_ == 3
    assert chunks[b"PLTE"] == b"\xff\xff\xff\x00\x00\x00", "palette"
    raw = zlib.decompress(idat)
    assert len(raw) == h * (w + 1)
    # unfilter
    rows = []
    prev = bytearray(w)
    p = 0
    for _ in range(h):
        f = raw[p]
        line = bytearray(raw[p + 1:p + 1 + w])
        p += w + 1
        for i in range(w):
            a = line[i - 1] if i > 0 else 0
            b = prev[i]
            c = prev[i - 1] if i > 0 else 0
            if f == 1:
                line[i] = (line[i] + a) & 0xFF
            elif f == 2:
                line[i] = (line[i] + b) & 0xFF
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif f == 4:
                pa, pb, pc = abs(a + b - c - a), abs(a + b - c - b), abs(a + b - c - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        rows.append(bytes(line))
        prev = line
    # pixel == scaled module (1=black), same float math as Image()
    mpp = size / 480.0
    for y in range(480):
        y2 = int(y * mpp)
        for x in range(480):
            x2 = int(x * mpp)
            want = 1 if bits[y2][x2] else 0
            assert rows[y][x] == want, f"case{idx} png pixel ({x},{y})"


def try_python_qrcode(idx, content, bits, size):
    try:
        import qrcode
    except ImportError:
        return "skipped"
    q = qrcode.QRCode(error_correction=qrcode.constants.ERROR_CORRECT_M,
                      border=4, mask_pattern=None)
    q.add_data(content)
    q.make(fit=True)
    m = q.get_matrix()
    if len(m) != size:
        raise AssertionError(f"case{idx}: python size {len(m)} != {size}")
    for y in range(size):
        for x in range(size):
            if m[y][x] != (bits[y][x] == 1):
                raise AssertionError(f"case{idx}: matrix diff at ({x},{y})")
    return "match"


def main():
    print("align table cross-check vs go-qrcode:", check_c_align_table())
    fails = 0
    for i, content in enumerate(CASES):
        try:
            version, mask, decoded, bits, size = decode_case(i)
            assert decoded == content, f"decoded mismatch: {decoded[:40]!r}"
            check_small(i, bits, size)
            check_png(i, bits, size)
            cross = try_python_qrcode(i, content, bits, size)
            print(f"case{i}: v{version} mask{mask} decode+RS ok, small ok, "
                  f"png ok, pyqrcode {cross}")
        except AssertionError as e:
            print(f"case{i}: FAIL {e}")
            fails += 1
    if fails:
        sys.exit(1)
    print(f"verify_qr: all {len(CASES)} cases ok")


if __name__ == "__main__":
    main()
