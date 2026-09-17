#!/usr/bin/env python3
"""Read the input server's EV_HEAD/EV_TAIL/CONSUMER_EP from guest RAM.

The input server loads at phys 0x3bf4000 (VA 0x1000000), so a symbol's
phys address = 0x3bf4000 + (VA - 0x1000000). Read the queue indices before
and after injecting mouse moves to see whether the wserver ever drains.
"""
import json
import socket
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

# input server: loaded phys 0x3bf4000, VA base 0x1000000
PHYS_BASE = 0x3bf4000
VA_BASE = 0x1000000
# symbol VAs (rust-nm)
CONSUMER_EP_VA = 0x1005000
EV_HEAD_VA = 0x1006048
EV_TAIL_VA = 0x1006050
# vring: Q0_RING at 0x1007000; used ring at +0x2000 (RING_USED_OFF),
# used.idx at offset 2 within VringUsed
USED_IDX_VA = 0x1007000 + 0x2000 + 2
AVAIL_IDX_VA = 0x1007000 + 0x1000 + 2

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


def read_state(q, tag):
    vals = {}
    for name, va in (("consumer", CONSUMER_EP_VA), ("head", EV_HEAD_VA),
                     ("tail", EV_TAIL_VA), ("usedidx", USED_IDX_VA),
                     ("availidx", AVAIL_IDX_VA)):
        pa = PHYS_BASE + (va - VA_BASE)
        r = qmp_cmd(q, {"execute": "human-monitor-command",
                        "arguments": {"command-line": "pmemsave %d 8 target/%s.bin"
                                       % (pa, name)}})
        if r is None or "return" not in r:
            print("pmemsave %s failed: %r" % (name, r), flush=True)
            continue
        time.sleep(0.2)
        with open("target/%s.bin" % name, "rb") as f:
            data = f.read()
        import struct
        vals[name] = struct.unpack("<q", data[:8])[0]
    print("%s: %s" % (tag, vals), flush=True)
    return vals


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

read_state(q, "before")

for i in range(4):
    qmp_cmd(q, {"execute": "input-send-event", "arguments": {
        "device": "fb0",
        "events": [{"type": "rel", "data": {"axis": "x", "value": 20}},
                   {"type": "rel", "data": {"axis": "y", "value": 15}}]
    }})
    time.sleep(0.3)
time.sleep(1.5)

read_state(q, "after-inject")

# Now run keytest — if it drains the queued events, head catches up.
send_paced("keytest /dev/kbd 40")
time.sleep(3)
read_state(q, "after-keytest")

q.close()
qemu.kill()
with lock:
    full = bytes(out).decode(errors="replace")
for line in full.splitlines():
    if "key " in line or "keytest" in line or "wdemo" in line:
        print("GUEST:", line, flush=True)
