#!/usr/bin/env python3
"""M5 gate: the panel is a layer surface, and its launcher takes a click.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`.
The panel is started by the session: `/etc/rc` starts `/bin/session`, which waits
for the shell's prompt and then starts `/etc/session`'s list, so nothing is typed
here. The panel is a `zwlr_layer_shell_v1` strip at the output's right edge with
one button; the probe moves the absolute pointer into that button, clicks it, and
expects the client's launch line and then the app's.

The app's window covers the left of the output and the panel is on the right, so
the second click is a plain repeat. The pointer also owns the keyboard focus, so
leaving it over the panel and pressing Escape ends the panel.

The panel's placement is checked on the display, not only through the protocol: a
QMP screendump has to show the strip at the right edge with the boot pattern's red
still at the origin.

Exit 0 on success, 1 otherwise.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4475
# The output and the panel's strip at its right edge (see `/bin/wlpanel`).
OUT_W = 1024
PANEL_W = 200

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none", "-display", "none",
    "-m", "256M", "-no-reboot", "-vga", "none",
    "-device", "bochs-display,id=fb0",
    "-qmp", "tcp:127.0.0.1:%d,server=on,wait=off" % QMP_PORT,
    "-kernel", "target/images/x86_64-pc-minix/minix-x86.elf",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
    "-device", "virtio-tablet-pci,display=fb0",
]

lock = threading.Lock()
out = bytearray()


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


def absolute(q, px, py):
    for axis, value, size in (("x", px, 1024), ("y", py, 768)):
        qmp_cmd(q, {
            "execute": "input-send-event",
            "arguments": {
                "device": "fb0",
                "events": [
                    {"type": "abs",
                     "data": {"axis": axis, "value": value * 0x8000 // size}}
                ],
            },
        })


def click(q, down):
    qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {
            "device": "fb0",
            "events": [{"type": "btn", "data": {"button": "left", "down": down}}],
        },
    })
    time.sleep(0.1)


def key(q, name, down):
    qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {"events": [
            {"type": "key", "data": {"key": {"type": "qcode", "data": name}, "down": down}}
        ]},
    })


SCREEN = "target/wlpanel.ppm"


def screendump(q):
    """One frame as `(width, height, pixels)`, or `None`.

    QEMU answers `screendump` before the file is complete, so the read waits on the
    size the PPM header promises rather than on a sleep.
    """
    try:
        os.unlink(SCREEN)
    except OSError:
        pass
    if qmp_cmd(q, {"execute": "screendump", "arguments": {"filename": SCREEN}}) is None:
        return None
    deadline = time.time() + 10
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


def pxat(screen, x, y):
    w, _h, px = screen
    off = (y * w + x) * 3
    return tuple(px[off:off + 3])


def main():
    global qemu
    qemu = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    threading.Thread(target=pump, daemon=True).start()

    ok = True

    if not wait_for(lambda: b"wlserver: ready" in out, 150):
        print("WLSERVER NOT READY", file=sys.stderr)
        with lock:
            print(bytes(out[-800:]).decode(errors="replace"), file=sys.stderr)
        qemu.kill()
        return 1
    if not wait_for(lambda: b"# " in out, 30):
        print("NO SHELL PROMPT", file=sys.stderr)
        qemu.kill()
        return 1
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", QMP_PORT), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    def step(n, name, pred, timeout=40):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    step(1, "the panel is up", lambda: b"wlpanel: ready" in out)

    def count(needle):
        # No lock here: `wait_for` already holds it while it calls its predicate,
        # and a plain `threading.Lock` cannot be taken twice.
        return bytes(out).count(needle)

    # Where the panel's frame lands, checked on the display rather than through the
    # protocol: the server blits a commit at the surface's place in the scene, so the
    # strip has to appear at the output's right edge. A server that painted every
    # commit at (0, 0) put the panel in the top-left corner, where the pointer gates
    # could not see it (they hit-test the scene's geometry, not the pixels) and the
    # boot pattern's red was gone from the origin.
    boot_red = (255, 0, 0)
    boot_blue = (0, 0, 255)
    painted = False
    for _ in range(10):
        if not ok:
            break
        screen = screendump(q)
        if screen is not None:
            painted = (pxat(screen, 0, 0) == boot_red
                       and pxat(screen, OUT_W - PANEL_W + 150, 16) != boot_blue)
            if painted:
                break
        time.sleep(0.5)
    step(2, "the panel is drawn at the output's right edge", lambda: painted)

    # The session starts the panel, so nothing is typed here. Into the launcher
    # button, which is 4..76 by 4..28 in panel coordinates: the panel is 200 wide
    # at the output's right edge, so the click lands at 1024 - 200 + 20.
    if ok:
        absolute(q, OUT_W - PANEL_W + 20, 16)
        time.sleep(1.0)
        click(q, True)
        click(q, False)
    step(3, "the launcher took the click",
         lambda: count(b"wlpanel: launch") >= 1)
    step(4, "the launcher started the app", lambda: b"wldemo: ready" in out)

    # The app's window covers the left of the output; the panel is on the right.
    # Moving a little and clicking again must still reach the launcher.
    if ok:
        absolute(q, OUT_W - PANEL_W + 30, 16)
        time.sleep(1.0)
        click(q, True)
        click(q, False)
    step(5, "a second click reached the panel again",
         lambda: count(b"wlpanel: launch") >= 2)
    step(6, "the second app started", lambda: count(b"wldemo: ready") >= 2)

    # The app the second click started took the focus when it mapped.
    if ok:
        key(q, "esc", True)
        time.sleep(0.1)
        key(q, "esc", False)
    step(7, "the second app finished", lambda: count(b"wldemo: PASS") >= 1, 20)

    # Nothing is focused now; a small move over the panel is what hands it back.
    if ok:
        absolute(q, OUT_W - PANEL_W + 40, 16)
        time.sleep(1.0)
        key(q, "esc", True)
        time.sleep(0.1)
        key(q, "esc", False)
    step(8, "the panel finished", lambda: b"wlpanel: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-3000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlpanel probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
