#!/usr/bin/env python3
"""The 3d-b-5 gate: two dma-buf surfaces composed on the GPU by a helper process and presented.

`/bin/wlscene` commits **two** render-node objects as `zwp_linux_dmabuf_v1` buffers on one
connection — a full-output red background and a 128x128 green overlay at the output's origin.
`/sbin/wlserver` keeps them in its scene (3d-b-5c); once there is more than one surface it asks
`/bin/wlcompose serve` — a separate, stateless Mesa process on the same render node — to draw them
into one frame, imports that frame, and scans it out (`FBIO_SCANOUT_SRC`). The first commit, with
a lone surface, stays on the per-commit fast path.

Evidence checked here:
  * both commits reach `wlscene: PASS`, so the compositor presented the frames they are in;
  * the fb server took the scan-out path (`fb: scanout src`, no `refused`) — the frame had no copy;
  * a QMP screendump shows the overlay's green at the origin over the background's red *everywhere
    else* — including where the boot pattern's green and blue were. That is a two-surface frame:
    the compositor's per-commit present could only have shown one source plane, and the server's
    own buffer holds the boot pattern, not the background. The compositor's `wlserver: composed N
    surfaces` line is printed too, and reported when it survives the console (a GL display drops
    console bytes, so the pixels, not the line, are the proof).

Like the other Mesa gates this needs a GL-capable dev host (headless CI has no GL). The devices
are the 4d/5b gates' — a 2D `virtio-gpu-pci` first and a GL device second — with `-display
egl-headless` for the GL device; the screendump captures the 2D device's scanout, where the
re-pointed frame lands.
"""
import json
import socket
import subprocess
import sys
import threading
import time

SCREEN = "target/wlscene.ppm"

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

    A GL display drops console bytes mid-line, and a truncated line still *runs*, as a shorter
    command. So each attempt is paced, and checked by the full echo before it counts.
    """
    want = cmd.encode()
    for _ in range(tries):
        with lock:
            offset = len(out)
        send_paced(cmd)
        if wait_for(lambda: want in bytes(out[offset:]), 25):
            return True
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
GREEN = bytes([0, 255, 0])


def main():
    global qemu
    argv = sys.argv[1:]
    if not argv:
        print("usage: wlscene_probe.py <qemu command...>", file=sys.stderr)
        sys.exit(2)
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    def die(msg):
        with lock:
            print("%s; tail:\n%s" % (msg, bytes(out[-2000:]).decode(errors="replace")),
                  file=sys.stderr)
        qemu.kill()
        sys.exit(1)

    if not wait_for(lambda: b"# " in out, 240):
        die("NO PROMPT")
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4447), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    # Baseline: the fb server's boot RGB pattern, so we know the scanout is live before anything
    # re-points it.
    base = screendump(q)
    if base is None:
        die("BASELINE SCREENDUMP FAILED")
    base_ok = (pxat(base, 0, 0) == RED
               and pxat(base, 512, 100) == GREEN
               and pxat(base, 1023, 767) == bytes([0, 0, 255]))
    print("baseline rgb: %s" % base_ok, flush=True)
    if not base_ok:
        die("BASELINE RGB MISMATCH")

    # The helper in the background, then the two-surface client. The shell's `&` forks the
    # helper; `wlserver` connects to it lazily on the first multi-surface frame.
    if not type_line("/bin/wlcompose serve &"):
        die("could not type /bin/wlcompose serve & — the console dropped bytes")
    time.sleep(1.5)
    if not type_line("/bin/wlscene"):
        die("could not type /bin/wlscene — the console dropped bytes")

    if not wait_for(lambda: b"wlscene: PASS" in out, 150):
        die("wlscene did not reach PASS")
    composed = b"wlserver: composed 2 surfaces" in out
    took_path = (b"fb: scanout src" in out) and (b"fb: scanout src refused" not in out)
    with lock:
        data = bytes(out)
    # A GL display drops console bytes mid-line, so the captured stream is kept whole for a
    # failed run to be read back without the terminal's mangling.
    with open("target/wlscene.log", "wb") as f:
        f.write(data)
    for line in data.splitlines():
            if (b"wlscene:" in line or b"wlserver:" in line or b"fb: scanout" in line
                    or b"wlcompose:" in line):
                print("serial: %s" % line.decode(errors="replace"), flush=True)
    print("composed 2 surfaces (serial, lossy): %s" % composed, flush=True)
    print("fb took the scanout path (no copy): %s" % took_path, flush=True)

    time.sleep(0.5)
    shot = screendump(q)
    if shot is None:
        die("FINAL SCREENDUMP FAILED")
    # The overlay is a 128x128 square at the origin; the background is red everywhere else, and
    # the boot pattern's green (512,100) and blue (1023,767) are gone with it.
    overlay = pxat(shot, 2, 2) == GREEN
    bg_centre = pxat(shot, 512, 384) == RED
    bg_boot_green = pxat(shot, 512, 100) == RED
    bg_boot_blue = pxat(shot, 1023, 767) == RED
    print("after composition: overlay green=%s, background red = %s centre, %s at the boot green,"
          " %s at the boot blue" % (overlay, bg_centre, bg_boot_green, bg_boot_blue), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (took_path and overlay and bg_centre and bg_boot_green and bg_boot_blue) else 1)


if __name__ == "__main__":
    main()
