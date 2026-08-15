#!/usr/bin/env python3
"""K4 real-system probe: framebuffer console (fbterm) with PS/2 echo.

Boots with QMP + bochs-display, runs `/bin/fbterm 2` (the fb console
exiting after 2 echoed keys), injects `a` and `b` via the monitor, and
asserts via QMP screendump that the glyphs are visible on the framebuffer:
the `> ` prompt at the top-left, then `a` at cell (2,0) and `b` at cell
(3,0) in the classic VGA 8x16 font, on the dark-blue background.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display",
    "-m", "256M", "-no-reboot",
    "-qmp", "tcp:127.0.0.1:4447,server=on,wait=off",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]

SCREEN = "tools/screen.ppm"

# fbterm's palette: BG = [0x18, 0x18, 0x30, 0] (B,G,R,X) -> PPM (R,G,B).
BG = bytes([0x30, 0x18, 0x18])
FG = bytes([0xFF, 0xFF, 0xFF])

lock = threading.Lock()
out = bytearray()


def pump():
    try:
        while True:
            chunk = qemu.stdout.read1(65536)
            if not chunk:
                break
            with lock:
                out.extend(chunk)
    finally:
        pass


def qmp_cmd(s, obj):
    s.sendall((json.dumps(obj) + "\n").encode())
    data = b""
    while True:
        chunk = s.recv(65536)
        data += chunk
        try:
            return json.loads(data.decode().splitlines()[-1])
        except Exception:
            if len(data) > (1 << 20):
                return None


def send_paced(cmd, per_byte_ms=3):
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def wait_for(pred, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


def screendump(s):
    r = qmp_cmd(s, {"execute": "screendump", "arguments": {"filename": SCREEN}})
    if r is None or r.get("return") is None:
        return None
    time.sleep(0.5)
    with open(SCREEN, "rb") as f:
        data = f.read()
    parts = data.split(b"\n", 3)
    dims = parts[1].split()
    w, h = int(dims[0]), int(dims[1])
    return w, h, parts[3]


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"# " in out, 150):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4447), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    # Start the fb console (exits after 2 echoed keys).
    send_paced("fbterm 2")
    if not wait_for(lambda: b"fbterm: ready" in out, 30):
        with lock:
            print("fbterm did not report ready; tail:\n%s"
                  % bytes(out[-800:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    def sendkey(k):
        qmp_cmd(q, {"execute": "human-monitor-command",
                    "arguments": {"command-line": "sendkey %s" % k}})

    sendkey("a")
    sendkey("b")

    if not wait_for(lambda: b"fbterm: done" in out, 30):
        with lock:
            print("fbterm did not exit; tail:\n%s"
                  % bytes(out[-800:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    screen = screendump(q)
    if screen is None:
        print("SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    w, h, px = screen
    print("screen: %dx%d" % (w, h), flush=True)

    def pxat(x, y):
        off = (y * w + x) * 3
        return px[off:off + 3]

    # '>' prompt stroke (row 2 of the glyph), 'a' at cell (2,0), 'b' at
    # cell (3,0); background rows/cols must stay the dark blue.
    prompt_ok = pxat(0, 2) == FG
    a_ok = pxat(17, 5) == FG
    b_ok = pxat(26, 5) == FG
    bg_ok = (pxat(16, 0) == BG and pxat(8, 0) == BG)
    print("prompt '>'=fg:%s  a-glyph=fg:%s  b-glyph=fg:%s  bg=%s"
          % (prompt_ok, a_ok, b_ok, bg_ok), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (prompt_ok and a_ok and b_ok and bg_ok) else 1)


if __name__ == "__main__":
    main()
