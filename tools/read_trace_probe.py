#!/usr/bin/env python3
"""Read-traffic probe v2: count virtio-blk I/Os for a boot + one command.

QEMU's `-trace ... -D file` (log backend) writes trace lines to a separate
file with no serial interleaving. Boot the same disk twice and run one
command each: the difference in trace-line counts isolates that command's
disk I/O. Exec'ing the 33 MiB no-op binary (`bign`) should add ~2-10 I/Os
(the ELF header blocks) if VFS's exec reads only the headers; a whole-image
read would add ~8400.
"""
import subprocess
import sys
import time

MEM = sys.argv[1] if len(sys.argv) > 1 else "256M"
CMD = sys.argv[2] if len(sys.argv) > 2 else "bign"
TRACE = sys.argv[3] if len(sys.argv) > 3 else "target/trace_probe.txt"

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-m", MEM, "-no-reboot",
        "-trace", "enable=virtio_blk_req_complete", "-D", TRACE,
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
)
out = bytearray()
prompt0 = 0
try:
    deadline = time.time() + 60
    while time.time() < deadline:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            if qemu.poll() is not None:
                break
            time.sleep(0.05)
            continue
        out += chunk
        if b"# " in out[-80:]:
            break
    qemu.stdin.write((CMD + "\n").encode())
    qemu.stdin.flush()
    deadline = time.time() + 60
    while time.time() < deadline:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            if qemu.poll() is not None:
                break
            time.sleep(0.05)
            continue
        out += chunk
        # Two prompts seen: the initial one and the one after the command.
        if out.count(b"# ") >= 2:
            break
    time.sleep(1)
finally:
    qemu.kill()
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass

text = bytes(out).decode("ascii", "replace")
# Windows console is cp1252; write raw bytes to avoid UnicodeEncodeError.
sys.stdout.buffer.write(bytes(out))
try:
    with open(TRACE, "r", errors="replace") as f:
        n = sum(1 for line in f if "virtio_blk_req_complete" in line)
except FileNotFoundError:
    n = -1
sys.stdout.buffer.write(("=== cmd=%s virtio_blk_req_complete count=%d ===\n" % (CMD, n)).encode())
