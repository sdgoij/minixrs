#!/usr/bin/env python3
"""The 3d-b-4c gate: a linux-dmabuf client's render-node object scanned out with no copy.

`/bin/wldmabuf` makes a full-output (1024x768 B8G8R8X8) object on the render node, fills it
green, exports it to a dma-buf, and commits it as a `zwp_linux_dmabuf_v1` buffer. `/sbin/wlserver`
imports the object on the same device and hands its frames to the fb server
(`FBIO_SCANOUT_SRC`), so the scanout *is* the client's object and the compositor copies
nothing (`.notes/wayland.md` 6.10, 3d-b-4). The frame is then the object, so `/dev/fb` (the
server's own buffer) no longer holds it; this probe checks the display from the host instead:
a QMP screendump must be the client's colour, and the serial log must carry the fb server's
`fb: scanout src` line proving the path was taken.

The QEMU line uses `virtio-gpu-pci` as the display and no `bochs-display`: the render node is
served from the output device itself (D11), so the client's render resource and the scanout
are one device, which is what lets the scanout be re-pointed at it.
"""
import json
import socket
import subprocess
import sys
import threading
import time

SCREEN = "target/wldmabuf.ppm"

lock = threading.Lock()
out = bytearray()
qemu = None


def pump():
    while True:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            break
        with lock:
            out.extend(chunk)


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
    return int(dims[0]), int(dims[1]), parts[3]


def pxat(screen, x, y):
    w, h, px = screen
    off = (y * w + x) * 3
    return px[off:off + 3]


GREEN = bytes([0, 255, 0])


def main():
    global qemu
    argv = sys.argv[1:]
    if not argv:
        print("usage: wldmabuf_probe.py <qemu command...>", file=sys.stderr)
        sys.exit(2)
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"# " in out, 240):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4447), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    # Baseline: the fb server's boot RGB pattern, so we know the display is live before the client.
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

    send_paced("/bin/wldmabuf")
    if not wait_for(lambda: b"wldmabuf: PASS" in out, 60):
        with lock:
            print("wldmabuf did not reach PASS; tail:\n%s"
                  % bytes(out[-1500:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    took_path = (b"fb: scanout src" in out) and (b"fb: scanout src refused" not in out)
    with lock:
        for line in bytes(out).splitlines():
            if b"fb: scanout" in line or b"wlserver:" in line:
                print("serial: %s" % line.decode(errors="replace"), flush=True)
    print("fb took the scanout path: %s" % took_path, flush=True)

    time.sleep(0.5)
    shot = screendump(q)
    if shot is None:
        print("FINAL SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    centre = pxat(shot, 512, 384) == GREEN
    corner = pxat(shot, 0, 0) == GREEN
    print("after commit: centre green=%s, corner green=%s" % (centre, corner), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (took_path and centre and corner) else 1)


if __name__ == "__main__":
    main()
