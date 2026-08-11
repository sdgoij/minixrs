#!/usr/bin/env python3
"""Drive QEMU exec-loop verification on any arch and RAM size.

Waits for the shell prompt, runs N external execs of `hello`, and verifies
the reported PIDs increment monotonically (file-region teardown + FDCLOSE
under repeated exec).

Usage:
  python3 tools/exec_loop_run.py x86 256M [N]
  python3 tools/exec_loop_run.py riscv64 1G
  python3 tools/exec_loop_run.py aarch64 4G

Requires the arch's image to be current: `just mkfs-<arch>` (target/disk.img)
and the kernel build for that arch (target/trampoline.elf + kernel.bin for
x86; target/<triple>/release/kernel-boot-<arch> otherwise).

A background thread pumps QEMU's serial output into a buffer so no wait ever
blocks on the pipe (Python's select does not support pipes on Windows) and
deadlines actually fire: a hung guest reports FAIL rather than hanging the
driver. The per-exec wait breaks on the "pid=" prefix and then sleeps 0.4 s,
matching the proven pacing of the original exec_loop_test.py — sending the
next command while the shell is still printing would race it. A final 3 s
drain lets the trailing digits of the last "pid=NN" line land before the
process is killed (otherwise "pid=31" can be truncated to "pid=3").
"""
import subprocess
import sys
import threading
import time
import re

ARCH = sys.argv[1] if len(sys.argv) > 1 else "x86"
MEM = sys.argv[2] if len(sys.argv) > 2 else "256M"
N = int(sys.argv[3]) if len(sys.argv) > 3 else 20

BASE = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-m", MEM, "-no-reboot",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]
RISCV = [
    "qemu-system-riscv64", "-machine", "virt", "-m", MEM, "-nographic",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0",
    "-kernel", "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64",
]
AARCH64 = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57",
    "-m", MEM, "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]
CMDS = {"x86": BASE, "riscv64": RISCV, "aarch64": AARCH64}

qemu = subprocess.Popen(
    CMDS[ARCH], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
)

lock = threading.Lock()
out = bytearray()
done = threading.Event()


def pump():
    try:
        while True:
            chunk = qemu.stdout.read1(65536)
            if not chunk:
                break
            with lock:
                out.extend(chunk)
    finally:
        done.set()


threading.Thread(target=pump, daemon=True).start()


def wait_for(pred, timeout):
    """Poll the buffer until pred() is true or the deadline passes."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


saw_prompt = wait_for(lambda: b"# " in out[-80:], 60)
if not saw_prompt:
    print("NO PROMPT within 60s — boot failed or hung", file=sys.stderr)
for i in range(N):
    qemu.stdin.write(b"hello\n")
    qemu.stdin.flush()
    saw = wait_for(lambda: b"pid=" in out[-120:], 10)
    time.sleep(0.4)  # let the shell return to prompt before the next command
    print("exec %d: %s" % (i, "ok" if saw else "FAIL"), file=sys.stderr)
    if not saw:
        break

# Drain tail output so the final pid extraction sees complete lines (the
# last "pid=NN" may otherwise be truncated to "pid=N" when QEMU is killed
# right after the pid-prefix wait breaks).
time.sleep(3)

pids = [int(x) for x in re.findall(rb"pid=(\d+)", bytes(out))]
qemu.kill()
try:
    qemu.wait(timeout=5)
except subprocess.TimeoutExpired:
    pass

text = bytes(out).decode("ascii", "replace")
print(text)
print("=== arch=%s mem=%s execs seen: %d, pids: %s ===" % (ARCH, MEM, len(pids), pids))
distinct = len(set(pids))
mono = all(pids[i] < pids[i + 1] for i in range(len(pids) - 1))
ok_execs = sum(1 for p in pids if p >= 11)
print("=== RESULT: %s ===" % ("PASS" if saw_prompt and ok_execs >= N and mono else "FAIL"))
