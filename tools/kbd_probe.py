#!/usr/bin/env python3
"""Inject a PS/2 keyboard event and watch the input ring advance."""
import json
import socket
import struct
import subprocess
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display,id=fb0",
    "-m", "256M", "-no-reboot",
    "-qmp", "tcp:127.0.0.1:4460,server=on,wait=off",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    "-device", "virtio-mouse-pci,display=fb0",
]

PHYS_BASE = 0x3bf4000
VA_BASE = 0x1000000
EV_HEAD_VA = 0x1006048
EV_TAIL_VA = 0x1006050

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


def read_u64(q, name, va):
    pa = PHYS_BASE + (va - VA_BASE)
    r = qmp_cmd(q, {"execute": "human-monitor-command",
                    "arguments": {"command-line": "pmemsave %d 8 target/%s.bin"
                                   % (pa, name)}})
    if r is None or "return" not in r:
        return None
    time.sleep(0.15)
    with open("target/%s.bin" % name, "rb") as f:
        return struct.unpack("<q", f.read(8))[0]


qemu = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT)
threading.Thread(target=pump, daemon=True).start()
wait_for(lambda: b"wserver: ready" in out, 150)
wait_for(lambda: b"# " in out, 30)
time.sleep(0.5)

q = socket.create_connection(("127.0.0.1", 4460), timeout=5)
qmp_cmd(q, {"execute": "qmp_capabilities"})
send_paced("wdemo info")
time.sleep(1.5)

print("head=%s tail=%s" % (read_u64(q, "h", EV_HEAD_VA), read_u64(q, "t", EV_TAIL_VA)), flush=True)
# Send a key press+release to the PS/2 keyboard.
for down in (True, False):
    r = qmp_cmd(q, {"execute": "input-send-event", "arguments": {
        "events": [{"type": "key", "data": {"key": {"type": "qcode", "data": "a"}, "down": down}}]
    }})
    print("key %s -> %s" % (down, r), flush=True)
    time.sleep(0.5)
time.sleep(2)
print("head=%s tail=%s" % (read_u64(q, "h", EV_HEAD_VA), read_u64(q, "t", EV_TAIL_VA)), flush=True)

q.close()
qemu.kill()
