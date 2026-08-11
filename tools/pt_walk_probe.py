#!/usr/bin/env python3
"""Find which live process (if any) still maps the leaked pages.

Boots x86 with QMP, runs N `hello` execs, then walks every live
process's page table from the kernel proc table (PROC_TABLE_ALIGNED @
0x33ce00, Proc stride 880, p_cr3 @ +256, p_rts_flags @ +304 [bit0 =
SLOT_FREE], p_name @ +488, p_endpoint @ +504, p_magic @ +748 =
0xC0FFEE1) and reports which processes map the leaked top-of-memory
pages. Also dumps the allocator bitmap for the leak addresses.

Usage:
  python3 tools/pt_walk_probe.py [N] [MEM]
"""
import json
import socket
import struct
import subprocess
import sys
import threading
import time

N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
MEM = sys.argv[2] if len(sys.argv) > 2 else "256M"

PROC_TABLE = 0x33CE00
STRIDE = 880
PROCS = 261
P_CR3 = 256
P_RTS = 304
P_NAME = 496
P_EP = 512
P_MAGIC = 788
PMAGIC = 0xC0FFEE1
SLOT_FREE = 1

PG_P = 0x001
PG_U = 0x004
PG_PS = 0x080
PG_FRAME = 0x000FFFFFFFFFF000

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-m", MEM, "-no-reboot",
        "-qmp", "tcp:127.0.0.1:4444,server,nowait",
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
)

lock = threading.Lock()
out = bytearray()


def pump():
    while True:
        c = qemu.stdout.read1(65536)
        if not c:
            break
        with lock:
            out.extend(c)


threading.Thread(target=pump, daemon=True).start()


def wait_for(pred, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


def send(cmd):
    t = threading.Thread(target=lambda: (qemu.stdin.write(cmd.encode() + b"\n"),
                                         qemu.stdin.flush()), daemon=True)
    t.start()
    t.join(3)


def pmemsave(phys, size, path):
    q = socket.create_connection(("127.0.0.1", 4444), timeout=10)
    q.settimeout(20)
    f = q.makefile("rb")
    f.readline()
    q.sendall(b'{"execute":"qmp_capabilities"}\n')
    f.readline()
    q.sendall((json.dumps({
        "execute": "pmemsave",
        "arguments": {"val": phys, "size": size, "filename": path},
    }) + "\n").encode())
    r = json.loads(f.readline().decode())
    q.close()
    return r


def read_phys(phys, size):
    path = "target/pt_walk.bin"
    pmemsave(phys, size, path)
    with open(path, "rb") as fh:
        return fh.read()


def walk_pagetable(cr3, want_phys):
    """Walk cr3's user half; return True if want_phys is mapped (leaf)."""
    root = struct.unpack_from("<512Q", read_phys(cr3, 4096))
    # User half on x86: entries 0..255 (each 512 GiB) covers 0..128 GiB.
    for i in range(0, 256):
        e4 = root[i]
        if e4 & PG_P == 0:
            continue
        if e4 & PG_PS:
            continue  # 1 GiB leaf
        pdpt = struct.unpack_from("<512Q", read_phys(e4 & PG_FRAME, 4096))
        for j in range(0, 512):
            e3 = pdpt[j]
            if e3 & PG_P == 0:
                continue
            if e3 & PG_PS:
                continue
            pd = struct.unpack_from("<512Q", read_phys(e3 & PG_FRAME, 4096))
            for k in range(0, 512):
                e2 = pd[k]
                if e2 & PG_P == 0:
                    continue
                if e2 & PG_PS:
                    # 2 MiB leaf — only want 4K pages, skip
                    continue
                pt = struct.unpack_from("<512Q", read_phys(e2 & PG_FRAME, 4096))
                for l in range(0, 512):
                    e1 = pt[l]
                    if e1 & PG_P == 0:
                        continue
                    if (e1 & PG_FRAME) == want_phys:
                        return True
    return False


def live_procs():
    data = read_phys(PROC_TABLE, STRIDE * PROCS)
    procs = []
    for s in range(PROCS):
        base = s * STRIDE
        rts = struct.unpack_from("<I", data, base + P_RTS)[0]
        if rts & SLOT_FREE:
            continue
        magic = struct.unpack_from("<I", data, base + P_MAGIC)[0]
        if magic != PMAGIC:
            continue
        cr3 = struct.unpack_from("<Q", data, base + P_CR3)[0]
        name = data[base + P_NAME:base + P_NAME + 16].split(b"\x00")[0].decode("ascii", "replace")
        ep = struct.unpack_from("<i", data, base + P_EP)[0]
        procs.append((s, ep, name, cr3, rts))
    return procs


def dump_raw():
    data = read_phys(PROC_TABLE, STRIDE * 4)
    nz = sum(1 for b in data if b != 0)
    print("raw first %d bytes at 0x%x: %d nonzero bytes" % (STRIDE * 4, PROC_TABLE, nz),
          file=sys.stderr, flush=True)
    for s in range(4):
        base = s * STRIDE
        rts = struct.unpack_from("<I", data, base + P_RTS)[0]
        cr3 = struct.unpack_from("<Q", data, base + P_CR3)[0]
        magic = struct.unpack_from("<I", data, base + P_MAGIC)[0]
        name = data[base + P_NAME:base + P_NAME + 16].split(b"\x00")[0]
        print("slot %d: rts=0x%x cr3=0x%x magic=0x%x name=%r" % (s, rts, cr3, magic, name),
              file=sys.stderr, flush=True)


try:
    saw_prompt = wait_for(lambda: b"# " in out[-80:], 60)
    print("prompt: %s" % saw_prompt, file=sys.stderr, flush=True)
    if not saw_prompt:
        sys.exit(1)
    time.sleep(0.5)
    for i in range(N):
        send("hello")
        if not wait_for(lambda: b"pid=" in out[-120:], 12):
            print("HANG after %d execs" % i, file=sys.stderr, flush=True)
            break
        if not wait_for(lambda: b"threadstd" in out[-400:], 10):
            print("HANG (no threadstd) after %d" % i, file=sys.stderr, flush=True)
            break
        if not wait_for(lambda: b"# " in out[-80:], 10):
            print("HANG (no prompt) after %d" % i, file=sys.stderr, flush=True)
            break
        time.sleep(0.3)
    time.sleep(1.0)

    # Sanity: the allocator struct read must work (it matched memstat before).
    inst = read_phys(0x587FA0, 32)
    bmp_ptr, bmp_len, top, free = struct.unpack_from("<4Q", inst, 0)
    print("sanity allocator: bitmap=0x%x len=%d top=%d free=%d" % (bmp_ptr, bmp_len, top, free),
          file=sys.stderr, flush=True)

    # Scan bss 0x22DDC0..0x588830 for the PMAGIC little-endian bytes.
    target = struct.pack("<I", 0x0C0FFEE1)
    found = 0
    for base in range(0x22E000, 0x588000, 0x40000):
        chunk = read_phys(base, 0x40000)
        idx = chunk.find(target)
        while idx != -1 and found < 5:
            print("PMAGIC bytes at 0x%08x" % (base + idx), file=sys.stderr, flush=True)
            found += 1
            idx = chunk.find(target, idx + 1)
    if found == 0:
        print("PMAGIC not found in bss range", file=sys.stderr, flush=True)

    dump_raw()
    procs = live_procs()
    print("live procs:", file=sys.stderr, flush=True)
    for s, ep, name, cr3, rts in procs:
        print("  slot %3d ep=%-5d %-12s cr3=0x%x rts=0x%x" % (s, ep, name, cr3, rts),
              file=sys.stderr, flush=True)

    # Leaked top-of-memory pages seen in earlier probes.
    targets = [0x0fc9f000, 0x0fca0000, 0x0fca1000, 0x0fca3000, 0x0fca4000, 0x0fcc7000,
               0x03e7d000, 0x03e80000]
    for t in targets:
        holders = []
        for s, ep, name, cr3, rts in procs:
            if cr3 != 0 and walk_pagetable(cr3, t):
                holders.append("%s(ep=%d)" % (name, ep))
        print("0x%08x mapped by: %s" % (t, ", ".join(holders) if holders else "NOBODY"),
              file=sys.stderr, flush=True)
finally:
    qemu.kill()
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
