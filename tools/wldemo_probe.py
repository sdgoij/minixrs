#!/usr/bin/env python3
"""M4 gate: a window of widgets, driven by the keyboard.

Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
runs `/bin/wldemo` — a text field above a list above a button, drawn by the
software backend — and types into it.

The client prints its state after every change, so the probe asserts the
interaction step by step instead of guessing at sleeps: two letters reach the
field, Tab moves the focus to the list, two Downs select the third row, Tab
moves it to the button, Enter fires it, and Escape ends the client.

Exit 0 on success, 1 otherwise.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4474

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


def key(q, name, down, hold_ms=40):
    """Press or release one QEMU key by qcode."""
    qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {"events": [
            {"type": "key", "data": {"key": {"type": "qcode", "data": name}, "down": down}}
        ]},
    })
    time.sleep(hold_ms / 1000.0)


def tap(q, name):
    key(q, name, True)
    key(q, name, False)


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

    send_paced("/bin/wldemo")

    def step(n, name, pred, timeout=40):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    def report(focus, field, selected, button):
        # No trailing newline: the guest's tty turns `\n` into `\r\n`, so a
        # predicate that ended with one would never match the console.
        return ('wldemo: focus=%d field="%s" list=%d button=%d'
                % (focus, field, selected, button)).encode()

    step(1, "the window is up", lambda: b"wldemo: ready" in out)
    step(2, "the field starts focused and empty",
         lambda: report(0, "", 0, 0) in out)

    # The field has the focus first, so the letters go straight in.
    if ok:
        tap(q, "h")
        tap(q, "i")
    step(3, "the letters reached the field",
         lambda: report(0, "hi", 0, 0) in out)

    # Shift comes from the seat's modifiers event, so a held shift has to change
    # the byte the next key produces.
    if ok:
        key(q, "shift", True)
        tap(q, "a")
        key(q, "shift", False)
    step(4, "shift uppercased the letter it was held for",
         lambda: report(0, "hiA", 0, 0) in out)

    if ok:
        tap(q, "tab")
    step(5, "Tab moved the focus to the list",
         lambda: report(1, "hiA", 0, 0) in out)

    if ok:
        tap(q, "down")
        tap(q, "down")
    step(6, "two Downs moved the selection to the third row",
         lambda: report(1, "hiA", 2, 0) in out)

    if ok:
        tap(q, "tab")
    step(7, "Tab moved the focus to the button",
         lambda: report(2, "hiA", 2, 0) in out)

    if ok:
        tap(q, "ret")
    step(8, "Enter fired the button",
         lambda: report(2, "hiA", 2, 1) in out)

    if ok:
        tap(q, "esc")
    step(9, "the client finished", lambda: b"wldemo: PASS" in out, 20)

    q.close()
    if not ok:
        with lock:
            print(bytes(out[-3000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("wldemo probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
