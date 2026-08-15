#!/usr/bin/env python3
"""Check the fb server liveness: cat /dev/fb after boot."""
import subprocess
import sys
import threading
import time

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-display", "none", "-vga", "none", "-device", "bochs-display",
        "-m", "256M", "-no-reboot",
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
)

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


threading.Thread(target=pump, daemon=True).start()


def wait_for(pred, timeout, base=0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred(base):
                return True
        time.sleep(0.05)
    with lock:
        return pred(base)


def send_paced(cmd, per_byte_ms=3):
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def main():
    if not wait_for(lambda b: b"# " in out[b:], 120):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.5)
    b = len(out)
    send_paced("cat /dev/fb")
    time.sleep(2.0)
    with lock:
        chunk = bytes(out[b:])
    print("CAT /dev/fb: %r" % chunk[-200:])
    sys.exit(0)


if __name__ == "__main__":
    main()
