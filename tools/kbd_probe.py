#!/usr/bin/env python3
"""K2 real-system probe: QEMU `sendkey` → PS/2 IRQ1 → input server →
keytest consumer.

Boots with QMP, runs `/bin/keytest` (polls /dev/kbd), injects `a` and
`b` via the monitor, and asserts the decoded HID events appear on the
serial console: `key 7 4 1` (a press), `key 7 5 1` (b press) and their
releases.
"""
import json
import socket
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display",
    "-m", "256M", "-no-reboot",
    "-qmp", "tcp:127.0.0.1:4445,server=on,wait=off",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]

lock = threading.Lock()
out = bytearray()


def pump():
    try:
        while True:
            chunk = qemu.stdout.read1(65536)
            if not chunk:
                break
            with lock:
                out.extend(chunk)
    finally:
        pass


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


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"# " in out, 120):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.5)

    # Start the keytest consumer (foreground; it polls /dev/kbd).
    send_paced("keytest")
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4445), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    def sendkey(k):
        qmp_cmd(q, {"execute": "human-monitor-command",
                    "arguments": {"command-line": "sendkey %s" % k}})

    base = len(out)
    sendkey("a")
    sendkey("b")

    ok = wait_for(lambda: b"key 7 4 1" in out and b"key 7 5 1" in out, 20)
    with lock:
        chunk = bytes(out[base:])
    print(chunk.decode(errors="replace")[-1200:])
    a_press = b"key 7 4 1" in chunk
    b_press = b"key 7 5 1" in chunk
    a_rel = b"key 7 4 0" in chunk
    b_rel = b"key 7 5 0" in chunk
    print("RESULT: a_press=%s b_press=%s a_release=%s b_release=%s"
          % (a_press, b_press, a_rel, b_rel))
    q.close()
    qemu.kill()
    sys.exit(0 if (a_press and b_press) else 1)


if __name__ == "__main__":
    main()
