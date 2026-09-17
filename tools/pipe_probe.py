#!/usr/bin/env python3
"""Verify shell pipelines work (regression check for the pipe_refcounts fix)."""
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display",
    "-m", "256M", "-no-reboot",
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


def send_paced(cmd, per_byte_ms=2):
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


qemu = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
threading.Thread(target=pump, daemon=True).start()
if not wait_for(lambda: b"# " in out, 150):
    print("NO PROMPT", file=sys.stderr)
    qemu.kill()
    sys.exit(1)
time.sleep(0.5)
MARK = b"zzzpipe"
send_paced("echo zzzpipe | cat")
# The marker appears twice: once in the shell's echoed command line and
# once as cat's actual output. A broken pipe (EPIPE) would leave only the
# echo. Count occurrences to distinguish.
def marker_count():
    with lock:
        return out.count(MARK)

t0 = time.time()
while marker_count() < 2 and time.time() - t0 < 15:
    time.sleep(0.1)
if marker_count() >= 2:
    print("PIPELINE OK: `echo zzzpipe | cat` printed its input")
else:
    with lock:
        print("PIPELINE FAILED — output missing. serial tail:")
        print(bytes(out[-1500:]).decode(errors="replace"))
    qemu.kill()
    sys.exit(1)
with lock:
    print("--- serial tail ---")
    print(bytes(out[-800:]).decode(errors="replace"))
qemu.kill()
sys.exit(0)
