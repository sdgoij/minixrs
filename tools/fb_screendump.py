#!/usr/bin/env python3
"""Boot with bochs-display, screendump via QMP, parse the PPM and assert
the red/green/blue test pattern."""
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
    "-qmp", "tcp:127.0.0.1:4445,server=on,wait=off",
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
            if len(data) > 1 << 20:
                return None
            continue


def parse_ppm(path):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:2] == b"P6", "not a PPM: %r" % data[:4]
    parts = data.split(b"\n", 3)
    dims = parts[1].split()
    w, h = int(dims[0]), int(dims[1])
    maxval = int(parts[2])
    px = parts[3]
    assert maxval == 255
    return w, h, px


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    deadline = time.time() + 180
    while time.time() < deadline:
        with lock:
            data = bytes(out)
        if b"# " in data:
            break
        time.sleep(0.5)
    print("prompt seen: %s" % (b"# " in data), flush=True)
    time.sleep(2)

    s = socket.create_connection(("127.0.0.1", 4445), timeout=5)
    qmp_cmd(s, {"execute": "qmp_capabilities"})
    r = qmp_cmd(s, {"execute": "screendump", "arguments": {"filename": SCREEN}})
    print("screendump: %s" % r, flush=True)
    s.close()
    time.sleep(1)

    w, h, px = parse_ppm(SCREEN)
    print("screen: %dx%d" % (w, h), flush=True)

    def pxat(x, y):
        off = (y * w + x) * 3
        return px[off:off + 3]

    for x in (0, 341, 512, 683, 1023):
        print("  (%3d, 0)  -> rgb %s" % (x, pxat(x, 0).hex()), flush=True)
    for y in (100, 383, 767):
        print("  (512, %3d) -> rgb %s" % (y, pxat(512, y).hex()), flush=True)

    # Assert thirds: red @ x=0, green @ x=512, blue @ x=1023.
    r_ok = pxat(0, 0) == bytes([255, 0, 0])
    g_ok = pxat(512, 100) == bytes([0, 255, 0])
    b_ok = pxat(1023, 767) == bytes([0, 0, 255])
    print("red left: %s, green middle: %s, blue right: %s"
          % (r_ok, g_ok, b_ok), flush=True)
    sys.exit(0 if (r_ok and g_ok and b_ok) else 1)


if __name__ == "__main__":
    main()
