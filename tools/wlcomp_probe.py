#!/usr/bin/env python3
"""The 4d-2 gate: a stock `wl_shm` client through smithay's compositor to the scan-out.

`/bin/wlcomp` binds `/run/wlcomp`, forks, and leaves a smithay compositor (`CompositorState` +
`ShmState` + a `wl_output`) serving in the child (`.notes/wayland.md` 6.13, 4d-2). `/bin/wlshm`
is a stock `wayland-client`: it creates one memfd with two 1024x768 `Xrgb8888` frames, commits
them in turn, and waits for the first `wl_buffer.release`. The compositor draws each commit into
an offscreen target, reads the frame back (D18) and re-points the fb server's scan-out at it
(`FBIO_SCANOUT_SRC`), so the frame *is* the compositor's buffer and the display shows what
smithay composed.

`--renderer soft` runs the same gate with the compositor's own software renderer selected, which
is the gate for that renderer: every assertion below is the same, and the log has to carry
`wlcomp: renderer soft` so a fallback cannot pass for it.

`--client dmabuf` types `/bin/wldmabuf` instead, which commits `zwp_linux_dmabuf_v1` buffers made
from render-node objects: the frame the compositor draws is then the client's own object, and the
log has to carry `wlcomp: dmabuf imported by the <renderer> renderer` before the screendump is
believed. Its two colours are blue (body) and yellow (top band).

Evidence: the serial log's `fb: scanout src` line (the re-point was taken), the client's release
line (the compositor held and returned the buffer), and a QMP screendump. The screenshot is the
orientation check as well: the second frame is one colour with its *top* 100 rows another, so the
band at the top of the screen says the present path did not flip the frame. The output is a
`virtio-gpu-pci` with no bochs-display, because only a virtio-gpu can re-back its scanout (D11).
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

SCREEN = "target/wlcomp.ppm"

# The clients this gate can type, with the colours their second frame paints: `body` everywhere
# and `band` on its top rows. The shm client's band is the orientation check; the dma-buf client's
# colours are distinct so a stale frame from the other one cannot pass.
CLIENTS = {
    "shm": {
        "cmd": "/bin/wlshm",
        "pass": b"wlshm: PASS",
        "fail": b"wlshm: FAIL",
        "body": bytes([255, 0, 255]),
        "band": bytes([0, 255, 0]),
    },
    "dmabuf": {
        "cmd": "/bin/wldmabuf",
        "pass": b"wldmabuf: PASS",
        "fail": b"wldmabuf: FAIL",
        "body": bytes([0, 0, 255]),
        "band": bytes([255, 255, 0]),
    },
}

# The compositor returns to the shell as soon as its socket is bound; the client is typed next.
SERVER_BASE = "/bin/wlcomp"

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
    """Type `cmd` a byte at a time, slowly enough for this guest's console."""
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


def type_line_and_wait(cmd, want, fail_marker, timeout, attempts=3):
    """Type `cmd` at the prompt and wait for `want`, retyping a line the guest never echoed.

    The echo is what says whether the line arrived, because this console mangles bursts: a
    line the guest never echoed whole is typed again; one that *was* echoed is the program's
    own answer, and a `fail_marker` in it is reported rather than retried.
    """
    with lock:
        anchor = len(out)
    for _ in range(attempts):
        send_paced(cmd)
        wait_for(lambda: want in out or fail_marker in out[anchor:], timeout)
        with lock:
            seen = bytes(out[anchor:])
        if want in seen:
            return True
        if fail_marker in seen:
            return False
        if cmd.encode() in seen:
            return False
        wait_for(lambda: b"# " in out[anchor:], 5)
    return False


def read_screendump():
    """Read `SCREEN` once QEMU has finished writing it (the PPM header carries the size)."""
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            with open(SCREEN, "rb") as f:
                data = f.read()
        except OSError:
            data = b""
        parts = data.split(b"\n", 3)
        if len(parts) == 4 and parts[0] == b"P6":
            dims = parts[1].split()
            if len(dims) == 2:
                w, h = int(dims[0]), int(dims[1])
                if len(parts[3]) >= w * h * 3:
                    return w, h, parts[3]
        time.sleep(0.05)
    return None


def screendump(s):
    try:
        os.unlink(SCREEN)
    except OSError:
        pass
    r = qmp_cmd(s, {"execute": "screendump", "arguments": {"filename": SCREEN}})
    if r is None or r.get("return") is None:
        return None
    return read_screendump()


def pxat(screen, x, y):
    w, h, px = screen
    off = (y * w + x) * 3
    return px[off:off + 3]


def tail(n=1200):
    with lock:
        return bytes(out[-n:]).decode(errors="replace")


def main():
    global qemu
    usage = ("usage: wlcomp_probe.py [--renderer gl|soft] [--client shm|dmabuf] "
             "<qemu command...>")
    argv = sys.argv[1:]
    options = {"--renderer": "gl", "--client": "shm"}
    for option in list(options):
        if option in argv:
            i = argv.index(option)
            try:
                options[option] = argv[i + 1]
            except IndexError:
                print(usage, file=sys.stderr)
                sys.exit(2)
            del argv[i:i + 2]

    renderer = options["--renderer"]
    if renderer not in ("gl", "soft"):
        print("--renderer takes `gl` or `soft`", file=sys.stderr)
        sys.exit(2)
    client_name = options["--client"]
    if client_name not in CLIENTS:
        print("--client takes `shm` or `dmabuf`", file=sys.stderr)
        sys.exit(2)
    client = CLIENTS[client_name]
    server_cmd = SERVER_BASE if renderer == "gl" else f"{SERVER_BASE} {renderer}"
    if not argv:
        print(usage, file=sys.stderr)
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

    # Baseline: the fb server's boot RGB pattern, so the display is known live before any commit.
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

    if not type_line_and_wait(server_cmd, b"wlcomp: ready on", b"wlcomp: FAIL", 60):
        print("the compositor did not become ready; tail:\n%s" % tail(), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    with lock:
        started = bytes(out)
    which = ("wlcomp: renderer %s" % renderer).encode()
    if which not in started:
        print("the compositor did not report `%s`; tail:\n%s" % (which.decode(), tail()),
              file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    print("renderer: %s" % renderer, flush=True)

    if not type_line_and_wait(client["cmd"], client["pass"], client["fail"], 40):
        print("the client did not pass; tail:\n%s" % tail(), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    with lock:
        log = bytes(out)
    took_scanout = b"fb: scanout src" in log and b"fb: scanout src refused" not in log
    presented = b"wlcomp: presented" in log
    released = client["pass"].split(b":")[0] + b": wl_buffer released" in log
    imported = None
    if client_name == "dmabuf":
        imported = ("wlcomp: dmabuf imported by the %s renderer" % renderer).encode() in log
    for line in log.splitlines():
        text = line.decode(errors="replace")
        if text.startswith(("fb: scanout", "wlcomp:", "wlshm:", "wldmabuf:")):
            print("serial: %s" % text, flush=True)
    print("scanout re-pointed=%s presented=%s buffer released=%s imported=%s"
          % (took_scanout, presented, released,
             "n/a" if imported is None else imported), flush=True)

    time.sleep(0.5)
    shot = screendump(q)
    if shot is None:
        print("FINAL SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    # The second frame is `body` with its top 100 rows `band`. The client counts rows from the
    # top of its image, so an upright present path puts the band at the top of the screen; a
    # flipped one puts it at the bottom.
    top_left = pxat(shot, 0, 0) == client["band"]
    top_mid = pxat(shot, 512, 40) == client["band"]
    centre = pxat(shot, 512, 384) == client["body"]
    corner = pxat(shot, 1023, 767) == client["body"]
    print("after commit: band top-left=%s, band top-middle=%s, centre body=%s, corner body=%s"
          % (top_left, top_mid, centre, corner), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (took_scanout and presented and released and imported is not False
                   and top_left and top_mid and centre and corner) else 1)


if __name__ == "__main__":
    main()
