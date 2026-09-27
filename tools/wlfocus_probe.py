#!/usr/bin/env python3
"""Phase 2c gate: two Wayland clients, one focus, and the keys that follow it.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`, then
runs `/bin/wlx2` — which opens *two* connections, maps a window on each, and prints
where the focus went — and injects keys with QMP `input-send-event`.

The second window appears last, so it must take the focus: the first connection is told
`leave` and the second `enter`, and each is released its own buffer. A key must then
reach the *second*, focused connection only. Next the first connection maps another
window; that must move the focus back (`leave` on the second, `enter` on the first), and
the second key must follow it to the first connection. A key that reaches the wrong
client fails the gate, which is what turns "input goes to the focused surface" from an
inference into a measurement.

Unlike the tsv gates this cannot be driven through the shell's stdin: that is the serial
console, and the input server never sees it. The keys have to be real device events.

Exit 0 on success, 1 otherwise.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4471

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

    send_paced("/bin/wlx2")

    def step(n, name, pred, timeout=40):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    step(1, "two clients mapped, focus moved to the second", lambda: b"wlx2: ready" in out)

    if ok:
        key(q, True)
        time.sleep(0.2)
        key(q, False)
        step(2, "the first key reached the focused client", lambda: b"wlx2: key 30 to B" in out)

    step(3, "a second window moved the focus back", lambda: b"wlx2: focus A" in out)

    if ok:
        key(q, True)
        time.sleep(0.2)
        key(q, False)
        step(4, "the second key followed the focus", lambda: b"wlx2: key 30 to A" in out)

    step(5, "the client finished", lambda: b"wlx2: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-2000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlfocus probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
