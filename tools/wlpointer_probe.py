#!/usr/bin/env python3
"""M3 gate: the focus follows the window under the pointer.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
runs `/bin/wlpointer` — two connections, a full-output window below and a 64x64
window above it, both at the origin — and moves the absolute pointer.

Near the top-left the pointer is over both, so the small window on top must take
the focus; further out it is over only the big one, so the focus must come back.
A key is injected at each place, and the connection that receives it is the
window that actually holds the focus.

Exit 0 on success, 1 otherwise.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4473

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
    """Move the absolute pointer to (px, py), naming the display device.

    Without the device QEMU answers "Input handler not found for event type
    abs"; the bochs display is what routes the axis events to the tablet.
    """
    response = None
    for axis, value, size in (("x", px, 1024), ("y", py, 768)):
        response = qmp_cmd(q, {
            "execute": "input-send-event",
            "arguments": {
                "device": "fb0",
                "events": [
                    {"type": "abs",
                     "data": {"axis": axis, "value": value * 0x8000 // size}}
                ],
            },
        })
    return response


def key(q, down):
    return qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {"events": [
            {"type": "key", "data": {"key": {"type": "qcode", "data": "a"}, "down": down}}
        ]},
    })


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
    time.sleep(0.5)

    q = socket.create_connection(("127.0.0.1", QMP_PORT), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    send_paced("/bin/wlpointer")

    def step(n, name, pred, timeout=40):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    step(1, "the two windows are up", lambda: b"wlpointer: ready" in out)

    FOCUS_PREFIX = b"wlserver: focus -> slot "

    def focus_is(slot):
        """The most recent focus report names `slot`."""
        def pred():
            at = out.rfind(FOCUS_PREFIX)
            return at >= 0 and out[at + len(FOCUS_PREFIX):][:1] == bytes([0x30 + slot])
        return pred

    # Into the 64x64 window at the origin, then back out over the full-output
    # one. Each move is interpolated, so assert the handover it causes before
    # making the next one. A key injected right after a move is dropped by the
    # guest's input ring, so the single key comes last and proves only where the
    # focus ended up.
    if ok:
        absolute(q, 16, 16)
    step(2, "the small window took the focus", focus_is(1))
    if ok:
        absolute(q, 400, 300)
    step(3, "the big window took it back", focus_is(0))
    if ok:
        key(q, True)
        time.sleep(0.2)
        key(q, False)
    step(4, "the key followed the pointer", lambda: b"wlpointer: key to A" in out)

    step(5, "the client finished", lambda: b"wlpointer: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-2000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlpointer probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
