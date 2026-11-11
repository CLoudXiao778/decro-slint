#!/usr/bin/env python3
import os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__)))
import verify_qr as vq

DUMP = os.environ.get("NE_QR_DUMP", "/tmp/neqr")

idx = 1
lines = open(os.path.join(DUMP, f"case{idx}.txt")).read().splitlines()
_, _case, cver, cmask, csize = lines[0].split()
version = int(cver[1:])
mask = int(cmask[4:])
size = int(csize[4:])
inner = size - 8
bits = [[1 if ch == "#" else 0 for ch in row] for row in lines[1:]]
core = [row[4:4 + inner] for row in bits[4:4 + inner]]

fmap = vq.function_map(version)
blocks = vq.MEDIUM[version]["blocks"]
total_cw = sum(nb * cw for nb, cw, dc in blocks)
rem = vq.MEDIUM[version]["remainder"]
pos = vq.walk_positions(version, fmap)
stream = []
for (x, y) in pos:
    vv = core[y][x]
    if vq.mask_bit(mask, x, y):
        vv ^= 1
    stream.append(vv)
cw = [int("".join(str(b) for b in stream[i:i + 8]), 2)
      for i in range(0, total_cw * 8, 8)]

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
            datas[j].append(cw[p]); p += 1
ecdatas = [[] for _ in range(nblk)]
for i in range(max(ec_blks)):
    for j in range(nblk):
        if i < ec_blks[j]:
            ecdatas[j].append(cw[p]); p += 1

print(f"case{idx}: v{version} mask{mask}, {nblk} blocks")
for j in range(nblk):
    gen = vq.rs_generator(ec_blks[j])
    expect = vq.poly_mod(datas[j] + [0] * ec_blks[j], gen)
    expect = expect + [0] * (ec_blks[j] - len(expect))
    got = ecdatas[j]
    print(f"block{j}: data={datas[j][:8]}... ec={ec_blks[j]}")
    print(f"  C   EC: {got}")
    print(f"  py  EC: {expect}")
    if got == expect:
        print("  MATCH")
    else:
        diffs = [k for k in range(len(got)) if got[k] != expect[k]]
        print(f"  MISMATCH at {len(diffs)}/{len(got)} pos: {diffs[:20]}")
