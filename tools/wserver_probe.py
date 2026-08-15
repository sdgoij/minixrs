#!/usr/bin/env python3
"""K5 real-system probe: the window server (wserver) desktop.

Boots with QMP + bochs-display (wserver is a boot proc; idle until a
client connects), runs `/bin/wdemo info` (static info window) then
`/bin/wdemo term 2` (terminal window, exits after 2 keys), injects `a`
and `b` via sendkey, and asserts via QMP screendump:

- desktop background, info title (unfocused) + color strip,
- term title (focused, since it was created last),
- the `a` and `b` glyphs echoed into the focused term window,
  proving key routing reaches the focused window.
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
    "-qmp", "tcp:127.0.0.1:4448,server=on,wait=off",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]

SCREEN = "tools/screen.ppm"

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

    if not wait_for(lambda: b"wserver: ready" in out, 150):
        print("WSERVER NOT READY", file=sys.stderr)
        with lock:
            print(bytes(out[-600:]).decode(errors="replace"), file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    if not wait_for(lambda: b"# " in out, 30):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    q = socket.create_connection(("127.0.0.1", 4448), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    # Info window (becomes unfocused once the term window exists).
    send_paced("wdemo info")
    if not wait_for(lambda: b"wdemo: info ok" in out, 30):
        with lock:
            print("wdemo info failed; tail:\n%s"
                  % bytes(out[-600:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    # Terminal window (focused) waiting for routed keys.
    send_paced("wdemo term 2")
    if not wait_for(lambda: b"wdemo: term waiting" in out, 30):
        with lock:
            print("wdemo term did not start; tail:\n%s"
                  % bytes(out[-600:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    def sendkey(k):
        qmp_cmd(q, {"execute": "human-monitor-command",
                    "arguments": {"command-line": "sendkey %s" % k}})

    sendkey("a")
    sendkey("b")

    if not wait_for(lambda: b"wdemo: term ok" in out, 30):
        with lock:
            print("wdemo term did not exit; tail:\n%s"
                  % bytes(out[-600:]).decode(errors="replace"), file=sys.stderr)
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

    DESKTOP = bytes([40, 40, 40])
    TITLE_FOCUSED = bytes([64, 128, 192])
    TITLE_UNFOCUSED = bytes([32, 32, 64])
    WHITE = bytes([255, 255, 255])
    RED = bytes([255, 0, 0])
    GREEN = bytes([0, 255, 0])
    BLUE = bytes([0, 0, 255])

    # info window at (40,40) 320x200: title 16px, body at y=56.
    bg_ok = pxat(10, 400) == DESKTOP
    info_title_ok = pxat(100, 48) == TITLE_UNFOCUSED
    # Color strip (body y 150..174 -> screen 206..230).
    strip_ok = (pxat(60, 210) == RED and pxat(60, 218) == GREEN
                and pxat(60, 226) == BLUE)
    # term window at (440,40): focused title, body at y=56.
    term_title_ok = pxat(500, 48) == TITLE_FOCUSED
    # 'a' at cell (2,0) -> (457,61); 'b' at cell (3,0) -> (466,61).
    a_ok = pxat(457, 61) == WHITE
    b_ok = pxat(466, 61) == WHITE

    print("desktop=%s info-title(unfocused)=%s strip=%s term-title(focused)=%s a-glyph=%s b-glyph=%s"
          % (bg_ok, info_title_ok, strip_ok, term_title_ok, a_ok, b_ok), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (bg_ok and info_title_ok and strip_ok and term_title_ok and a_ok and b_ok) else 1)


if __name__ == "__main__":
    main()
