#!/usr/bin/env python3
"""M3 gate: the pointer is interpolated across a tick, not jumped.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
runs `/bin/wlmotion` — which maps one output-sized window, so its first commit
takes the focus and the pointer's events — and then moves the absolute pointer
with QMP `input-send-event`.

The move is one event at each axis, but the pointer must reach the client as a
stream of `wl_pointer.motion` events, one per tick, ending at the landing point.
The client prints how many it saw and where the last one was; this probe fails
when the count is a jump (two motions, one per axis) or the endpoint is off.

Exit 0 on success, 1 otherwise.
"""
import json
import re
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4472
# QEMU's absolute tablet is 0..0x7FFF over the output, and the server maps it
# back with `value * size / 0x8000`. These two land on (768, 480).
TARGET_X = 0x6000
TARGET_Y = 0x5000
EXPECT_X = TARGET_X * 1024 // 0x8000
EXPECT_Y = TARGET_Y * 768 // 0x8000
MIN_STEPS = 4

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


def absolute(q, axis, value):
    return qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {
            "device": "fb0",
            "events": [
                {"type": "abs", "data": {"axis": axis, "value": value}}
            ],
        },
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

    send_paced("/bin/wlmotion")

    def step(n, name, pred, timeout=40):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    step(1, "the client took the pointer", lambda: b"wlmotion: ready" in out)

    if ok:
        absolute(q, "x", TARGET_X)
        absolute(q, "y", TARGET_Y)

    step(2, "the motions arrived", lambda: b"wlmotion: steps=" in out)

    if ok:
        with lock:
            data = bytes(out)
        match = re.search(rb"wlmotion: steps=(\d+) x=(\d+) y=(\d+)", data)
        if match is None:
            print("[3] the client's report: FAIL", flush=True)
            ok = False
        else:
            steps, x, y = (int(value) for value in match.groups())
            print("[3] steps=%d x=%d y=%d: %s" % (
                steps, x, y,
                "ok" if steps >= MIN_STEPS
                and abs(x - EXPECT_X) <= 1 and abs(y - EXPECT_Y) <= 1
                else "FAIL"), flush=True)
            if steps < MIN_STEPS or abs(x - EXPECT_X) > 1 or abs(y - EXPECT_Y) > 1:
                ok = False

    step(4, "the client finished", lambda: b"wlmotion: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-2000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlmotion probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
