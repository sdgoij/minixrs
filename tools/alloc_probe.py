#!/usr/bin/env python3
"""Dump and diff the x86 kernel physical-page bitmap across repeated execs.

Reads the arch allocator's bitmap (GLOBAL_BITMAP @ 0x387fa0, allocator
struct @ 0x587fa0) via QMP pmemsave at boot and after N `hello` execs,
then reports which physical pages were allocated and never freed. This
pinpoints WHERE the per-exec leak lives (identity window vs. table pages
vs. scattered leaves).

Usage:
  python3 tools/alloc_probe.py [N] [MEM]
"""
import json
import re
import socket
import struct
import subprocess
import sys
import threading
import time

N = int(sys.argv[1]) if len(sys.argv) > 1 else 40
MEM = sys.argv[2] if len(sys.argv) > 2 else "256M"
S = int(sys.argv[3]) if len(sys.argv) > 3 else 40

ALLOC_INSTANCE_PHYS = 0x587FA0
GLOBAL_BITMAP_PHYS = 0x387FA0

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


def qmp_cmd(execute, args=None):
    q = socket.create_connection(("127.0.0.1", 4444), timeout=10)
    q.settimeout(20)
    f = q.makefile("rb")
    f.readline()
    q.sendall(b'{"execute":"qmp_capabilities"}\n')
    f.readline()
    body = {"execute": execute}
    if args:
        body["arguments"] = args
    q.sendall((json.dumps(body) + "\n").encode())
    r = json.loads(f.readline().decode())
    q.close()
    return r


def pmemsave(phys, size, path):
    return qmp_cmd("pmemsave", {"val": phys, "size": size, "filename": path})


def read_bitmap(tag):
    path = "target/alloc_bitmap.bin"
    pmemsave(ALLOC_INSTANCE_PHYS, 32, path)
    with open(path, "rb") as fh:
        inst = fh.read(32)
    bitmap_ptr, bitmap_len, top_page, free_pages = struct.unpack_from("<4Q", inst, 0)
    print("%s: struct bitmap_ptr=0x%x bitmap_len=%d top=%d free=%d" % (
        tag, bitmap_ptr, bitmap_len, top_page, free_pages),
        file=sys.stderr, flush=True)
    pmemsave(bitmap_ptr, bitmap_len * 8, path)
    with open(path, "rb") as fh:
        data = fh.read(bitmap_len * 8)
    pages = []
    for i, word in enumerate(struct.iter_unpack("<Q", data)):
        w = word[0]
        for b in range(64):
            if w & (1 << b):
                pages.append(i * 64 + b)
    print("%s: free=%d top=%d bitmap_len=%d setbits=%d allocated=%d pages" % (
        tag, free_pages, top_page, bitmap_len, len(pages), top_page - free_pages),
        file=sys.stderr, flush=True)
    return set(pages), free_pages


try:
    saw_prompt = wait_for(lambda: b"# " in out[-80:], 60)
    print("prompt: %s" % saw_prompt, file=sys.stderr, flush=True)
    if not saw_prompt:
        sys.exit(1)
    time.sleep(0.5)
    boot_pages, boot_free = read_bitmap("boot")
    hung = None
    total_done = 0
    # Sample every S execs so the per-exec leak rate is measured over
    # multiple intervals rather than one end point.
    prev_pages, prev_free = boot_pages, boot_free
    for batch in range(1, (N + S - 1) // S + 1):
        for i in range(S):
            send("hello")
            if not wait_for(lambda: b"pid=" in out[-120:], 12):
                hung = total_done
                print("HANG after %d execs" % total_done, file=sys.stderr, flush=True)
                break
            if not wait_for(lambda: b"threadstd" in out[-400:], 10):
                hung = total_done
                print("HANG (no threadstd) after %d execs" % total_done, file=sys.stderr, flush=True)
                break
            if not wait_for(lambda: b"# " in out[-80:], 10):
                hung = total_done
                print("HANG (no prompt) after %d execs" % total_done, file=sys.stderr, flush=True)
                break
            total_done += 1
            time.sleep(0.3)
        if hung is not None:
            break
        time.sleep(1.0)  # let in-flight cleanup settle
        cur_pages, cur_free = read_bitmap("after %d execs" % total_done)
        lost = len(prev_pages - cur_pages)
        print("interval %d->%d: free %d->%d, %d pages permanently used (%.2f/exec)" % (
            total_done - S, total_done, prev_free, cur_free, lost, lost / S),
            file=sys.stderr, flush=True)
        prev_pages, prev_free = cur_pages, cur_free
    done = total_done
    end_pages, end_free = prev_pages, prev_free
    leaked = sorted(boot_pages - end_pages)
    print("leaked pages: %d (%.1f/exec)" % (len(leaked), len(leaked) / max(done, 1)),
          file=sys.stderr, flush=True)
    # Group by 1 MiB window to show the allocation geography.
    from collections import Counter
    windows = Counter(p // 256 for p in leaked)
    print("by 1 MiB window:", file=sys.stderr, flush=True)
    for w, c in sorted(windows.items()):
        print("  0x%06x: %d pages" % (w * 0x100000, c), file=sys.stderr, flush=True)
    if len(leaked) <= 64:
        print("exact:", file=sys.stderr, flush=True)
        for p in leaked:
            print("  0x%08x" % (p * 0x1000), file=sys.stderr, flush=True)
    else:
        # Print the lowest and highest leaked pages to see the drift range.
        print("range: 0x%08x .. 0x%08x" % (leaked[0] * 0x1000, leaked[-1] * 0x1000),
              file=sys.stderr, flush=True)
finally:
    qemu.kill()
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
