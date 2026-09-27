#!/usr/bin/env python3
"""Phase 1c gate: a host key injected on the PS/2 keyboard reaches a Wayland client.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`, then
runs `/bin/wlkey` — which binds `wl_seat`'s keyboard, commits a surface so the
server sends `enter`, and prints each key it receives — and injects `a` with QMP
`input-send-event`.

The acceptance is the line `wlkey: key 30 pressed`. 30 is evdev's `KEY_A`, so a
match proves the whole path: the PS/2 controller queues an HID usage, the input
server's ring hands it to a *reader of its own* (wlserver, alongside wserver), wlserver
translates the usage to a keycode, and `wl_keyboard.key` carries it to the client.

Unlike the tsv gates this cannot be driven through the shell's stdin: that is the
serial console, and the input server never sees it. The key has to be a real device
event, which is what QMP injects.

Exit 0 on success, 1 otherwise.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4470

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

    send_paced("/bin/wlkey")
    if not wait_for(lambda: b"wlkey: ready" in out, 30):
        print("[1] wlkey ready: FAIL", flush=True)
        ok = False
    else:
        print("[1] wlkey ready: ok", flush=True)

    if ok:
        # Press then release, the way a keyboard sends a key.
        key(q, True)
        time.sleep(0.2)
        key(q, False)

        if wait_for(lambda: b"wlkey: key 30 pressed" in out, 30):
            print("[2] key 30 pressed reached the client: ok", flush=True)
        else:
            print("[2] key 30 pressed reached the client: FAIL", flush=True)
            ok = False

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-1500:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wlkey probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
