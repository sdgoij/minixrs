#!/usr/bin/env python3
"""The 3d-b-4d gate: a GL client whose render target *is* the linux-dmabuf buffer scanned out.

`/bin/wldmabufgl` renders a GLES2 triangle with Mesa on the render node into a GBM buffer
object (through an `EGLImage` over the BO's descriptor — `tools/egl_bo_probe.c` is the
measurement that this works on the port), exports it to a dma-buf, and commits it as a
`zwp_linux_dmabuf_v1` buffer. `/sbin/wlserver` imports the object on the render node and hands
its frames to the fb server (`FBIO_SCANOUT_SRC`), so the frame *is* the client's object and the
compositor copies nothing (`.notes/wayland.md` §6.10, 3d-b-4d).

Where this host departs from the design (`.notes/wayland.md` §5): its virgl renders into a
host-side texture, so the object's own pages only hold the frame after the client resolves it
and transfers it in — one copy, in the client. 3d-a read the *whole* surface back with
`glReadPixels`; 4d transfers it into the buffer the compositor scans out, which is the
compositor-side saving.

The frame is the client's object, so `/dev/fb` (the server's own buffer) no longer holds it; the
display is checked from the host instead, with a QMP screendump. The QEMU line uses `-display
egl-headless` (a GL device needs a GL display backend) with a **2D `virtio-gpu-pci` as the first
device**: the client renders on the GL node, and the screendump captures the 2D device's scanout,
where the re-pointed frame lands.

Evidence checked here:
  * the client reaches `wldmabufgl: PASS` and reports the exported object;
  * the fb server took the scan-out path (`fb: scanout src`, no `refused`) — the no-copy proof,
    since a frame it could not re-point would go through the compositor's blit instead;
  * the screendump shows the triangle: a red centroid over a black corner.
"""
import json
import socket
import subprocess
import sys
import threading
import time

SCREEN = "target/wldmabufgl.ppm"

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


def send_paced(cmd, per_byte_ms=20):
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def type_line(cmd, tries=4):
    """Type `cmd` and do not accept it until the guest has echoed the whole of it.

    A GL display drops console bytes mid-line (the reason `feed.sh` has `FEED_PACE`), and a
    truncated line still *runs*, as a shorter command — measured: `/bin/wldmabufgl` arrived as
    `/bi/wldmabufgl`. So each attempt is paced, and checked by the full echo before it counts.
    """
    want = cmd.encode()
    for _ in range(tries):
        with lock:
            offset = len(out)
        send_paced(cmd)
        if wait_for(lambda: want in bytes(out[offset:]), 25):
            return True
        # A shorter line ran; let the shell come back before the next attempt.
        time.sleep(1.5)
    return False


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


RED = bytes([255, 0, 0])
BLACK = bytes([0, 0, 0])


def main():
    global qemu
    argv = sys.argv[1:]
    if not argv:
        print("usage: wldmabufgl_probe.py <qemu command...>", file=sys.stderr)
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

    # Baseline: the fb server's boot RGB pattern, so we know the 2D scanout is live before the
    # client re-points it.
    base = screendump(q)
    if base is None:
        print("BASELINE SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    base_ok = (pxat(base, 0, 0) == RED
               and pxat(base, 512, 100) == bytes([0, 255, 0])
               and pxat(base, 1023, 767) == bytes([0, 0, 255]))
    print("baseline rgb: %s" % base_ok, flush=True)
    if not base_ok:
        print("BASELINE RGB MISMATCH", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    if not type_line("/bin/wldmabufgl"):
        print("could not type /bin/wldmabufgl — the console dropped bytes", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    if not wait_for(lambda: b"wldmabufgl: PASS" in out, 120):
        with lock:
            print("wldmabufgl did not reach PASS; tail:\n%s"
                  % bytes(out[-2000:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    took_path = (b"fb: scanout src" in out) and (b"fb: scanout src refused" not in out)
    with lock:
        for line in bytes(out).splitlines():
            if (b"wldmabufgl:" in line or b"fb: scanout" in line
                    or b"wlserver:" in line):
                print("serial: %s" % line.decode(errors="replace"), flush=True)
    print("fb took the scanout path (no copy): %s" % took_path, flush=True)

    time.sleep(0.5)
    shot = screendump(q)
    if shot is None:
        print("FINAL SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    centre = pxat(shot, 512, 384) == RED
    corner = pxat(shot, 0, 0) == BLACK
    print("after commit: centre red=%s, corner black=%s" % (centre, corner), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (took_path and centre and corner) else 1)


if __name__ == "__main__":
    main()
