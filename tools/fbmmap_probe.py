#!/usr/bin/env python3
"""K3 real-system probe: userland mmap of /dev/fb (char-device mmap).

Boots with QMP + bochs-display, runs `/bin/fbmmap paint` (mmaps /dev/fb
MAP_SHARED, paints a 200x200 white block top-left, read-backs pixels),
screendumps and asserts the white block is visible while the boot RGB
pattern is intact elsewhere, then runs `fbmmap restore` and asserts the
block is red again (the boot pattern) — proving the mapping aliases the
live framebuffer.
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
    "-qmp", "tcp:127.0.0.1:4446,server=on,wait=off",
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

    if not wait_for(lambda: b"# " in out, 150):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4446), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    def pxat(screen, x, y):
        w, h, px = screen
        off = (y * w + x) * 3
        return px[off:off + 3]

    # Baseline: the fb server's boot RGB pattern.
    base = screendump(q)
    if base is None:
        print("BASELINE SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    base_ok = (pxat(base, 0, 0) == bytes([255, 0, 0])
               and pxat(base, 512, 100) == bytes([0, 255, 0])
               and pxat(base, 1023, 767) == bytes([0, 0, 255]))
    print("baseline rgb: %s" % base_ok, flush=True)
    if not base_ok:
        print("BASELINE RGB MISMATCH", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    # Paint a white block top-left via a userland mmap of /dev/fb.
    send_paced("fbmmap paint")
    if not wait_for(lambda: b"fbmmap: paint ok" in out, 30):
        with lock:
            print("fbmmap paint did not report ok; tail:\n%s"
                  % bytes(out[-800:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    painted = screendump(q)
    if painted is None:
        print("PAINTED SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    white_ok = pxat(painted, 100, 100) == bytes([255, 255, 255])
    rgb_ok = (pxat(painted, 512, 100) == bytes([0, 255, 0])
              and pxat(painted, 1023, 767) == bytes([0, 0, 255]))
    print("after paint: white block=%s, rgb intact=%s"
          % (white_ok, rgb_ok), flush=True)

    # Restore the boot pattern (top-left third is red).
    send_paced("fbmmap restore")
    if not wait_for(lambda: b"fbmmap: restore ok" in out, 30):
        with lock:
            print("fbmmap restore did not report ok; tail:\n%s"
                  % bytes(out[-800:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    restored = screendump(q)
    if restored is None:
        print("RESTORED SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    red_ok = pxat(restored, 100, 100) == bytes([255, 0, 0])
    print("after restore: block red=%s" % red_ok, flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (white_ok and rgb_ok and red_ok) else 1)


if __name__ == "__main__":
    main()
