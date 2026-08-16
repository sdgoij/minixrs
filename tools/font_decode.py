#!/usr/bin/env python3
"""Decode the wterm window body from a PPM using the repo's 8x16 font."""
import re
import sys

FONT = "crates/minix-std/src/font.rs"
PPM = sys.argv[1] if len(sys.argv) > 1 else "tools/wt_bs.ppm"

# Parse FONT_8X16: 95 glyphs (ASCII 0x20..0x7E) x 16 rows of byte literals.
src = open(FONT, "r").read()
m = re.search(r"pub const FONT_8X16: \[\[u8; 16\]; 95\] = \[(.*?)\];", src, re.S)
if not m:
    print("font table not found")
    sys.exit(1)
body = m.group(1)
glyphs = []
for gm in re.finditer(r"\[(.*?)\]", body, re.S):
    vals = [int(x, 0) for x in re.findall(r"0x[0-9A-Fa-f]+", gm.group(1))]
    if len(vals) == 16:
        glyphs.append(vals)

# Load the PPM body region and build per-cell bitmasks.
data = open(PPM, "rb").read()
parts = data.split(b"\n", 3)
dims = parts[1].split()
w, h = int(dims[0]), int(dims[1])
px = parts[3]
WIN_X, WIN_Y = 120, 60

def cell_mask(c, r):
    mask = 0
    bit = 0
    for y in range(WIN_Y + 16 + r * 16, WIN_Y + 16 + r * 16 + 16):
        rowbits = 0
        for x in range(WIN_X + c * 8, WIN_X + c * 8 + 8):
            off = (y * w + x) * 3
            if px[off:off + 3] == b"\xff\xff\xff":
                rowbits |= 1 << (7 - (x - (WIN_X + c * 8)))
        mask |= rowbits << (16 * (y - (WIN_Y + 16 + r * 16)))
    return mask

def match(c, r):
    mask = cell_mask(c, r)
    best, bestn = " ", 0
    for i, g in enumerate(glyphs):
        gm = 0
        for j, row in enumerate(g):
            gm |= row << (16 * j)
        # exact match preferred; else highest overlap
        if gm == mask:
            return chr(i + 0x20)
        n = bin(gm & mask).count("1")
        if n > bestn:
            bestn = n
            best = chr(i + 0x20)
    return best

for r in range(4):
    line = "".join(match(c, r) for c in range(30))
    print("%02d|%s|" % (r, line))
