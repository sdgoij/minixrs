#!/usr/bin/env python3
"""M5 gate: the panel is a layer surface, and its launcher takes a click.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
runs `/bin/wlpanel` — a `zwlr_layer_shell_v1` surface across the top of the
output with one button — moves the absolute pointer into that button, clicks it,
and expects the client's launch line.

The pointer starts at the centre of the output, so moving into the panel is also
what gives it the focus: the click only reaches it if the server hit-tested the
layer surface and told the new owner where the pointer is.

Exit 0 on success, 1 otherwise.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4475

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

    send_paced("/bin/wlpanel")

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

    # Into the launcher button, which is 4..76 by 4..28 in panel coordinates. A
    # press outside it would not grab, and the release would go nowhere, so the
    # launch line is what proves the pointer's position reached the panel.
    if ok:
        absolute(q, 20, 16)
        time.sleep(1.0)
        click(q, True)
        click(q, False)
    step(2, "the launcher took the click", lambda: b"wlpanel: launch" in out)

    if ok:
        key(q, "esc", True)
        time.sleep(0.1)
        key(q, "esc", False)
    step(3, "the panel finished", lambda: b"wlpanel: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-3000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlpanel probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
