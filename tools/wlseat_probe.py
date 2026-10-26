#!/usr/bin/env python3
"""The 4d-3 gate: a host key reaches a stock `wayland-client` through smithay's seat.

`/bin/wlcomp` starts the 4d-2 compositor (now with a `wl_seat` and a keyboard) and `/bin/wlseat`
is a stock `wayland-client` that binds the seat, reads the keymap the seat serves, commits one
`wl_shm` frame so the compositor has a surface to focus, and waits. The host then injects `a`
with QMP `input-send-event`; the client must receive evdev code 30, and it already has the
keymap (`.notes/wayland.md` 6.13, 4d-3).

The key cannot come from the shell's stdin: that is the serial console, and the input server
never sees it. It has to be a device event, which is what QMP injects; the port's PS/2 path is
what `wlkey`'s gate already proved reaches the input server's ring.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

QMP_PORT = 4472

# The compositor returns to the shell as soon as its socket is bound; the client is typed next.
SERVER_CMD = "/bin/wlcomp"
CLIENT_CMD = "/bin/wlseat"

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


def inject_a(q, down):
    return qmp_cmd(q, {
        "execute": "input-send-event",
        "arguments": {"events": [
            {"type": "key", "data": {"key": {"type": "qcode", "data": "a"}, "down": down}}
        ]},
    })


def tail(n=1500):
    with lock:
        return bytes(out[-n:]).decode(errors="replace")


def main():
    global qemu
    argv = sys.argv[1:]
    if not argv:
        print("usage: wlseat_probe.py <qemu command...>", file=sys.stderr)
        sys.exit(2)
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"# " in out, 240):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    q = socket.create_connection(("127.0.0.1", QMP_PORT), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    if not type_line_and_wait(SERVER_CMD, b"wlcomp: ready on", b"wlcomp: FAIL", 90):
        print("the compositor did not become ready; tail:\n%s" % tail(), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    if not type_line_and_wait(CLIENT_CMD, b"wlseat: waiting for a key", b"wlseat: FAIL", 60):
        print("the client did not reach the seat; tail:\n%s" % tail(), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    # Press and release, the way a keyboard sends a key.
    inject_a(q, True)
    time.sleep(0.2)
    inject_a(q, False)

    if not wait_for(lambda: b"wlseat: PASS" in out, 30):
        print("the key did not reach the client; tail:\n%s" % tail(), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    with lock:
        log = bytes(out)
    keymap_ok = False
    for line in log.splitlines():
        text = line.decode(errors="replace")
        if text.startswith(("wlcomp:", "wlseat:")):
            print("serial: %s" % text, flush=True)
        if text.startswith("wlseat: keymap ") and text.endswith(" ok"):
            keymap_ok = int(text.split()[2]) > 0
    reached = b"wlseat: key 30 " in log
    print("keymap served=%s key 30 reached the client=%s" % (keymap_ok, reached), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (keymap_ok and reached) else 1)


if __name__ == "__main__":
    main()
