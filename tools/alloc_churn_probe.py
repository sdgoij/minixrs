#!/usr/bin/env python3
"""Drive QEMU: run `/bin/allocprobe` and check the VM region count stays
flat under alloc/free churn.

`allocprobe` (crates/userland/src/bin/allocprobe.rs, embedded via
crates/boot-image/src/manifest.rs) drives the rt mmap allocator directly:
grow the heap across several chunks then release it (the fully-free
chunks must be munmapped), then churn 200 x 1 KiB blocks inside one chunk
(freed blocks must be reused, so the region count never grows). It prints
`allocprobe: PASS ...` or `allocprobe: FAIL ...` to the serial console.

Usage:
  python3 tools/alloc_churn_probe.py [ARCH] [MEM]
  ARCH = x86 (default) | riscv64 | aarch64

Requires a current image: `just build-<arch>` + `just mkfs-<arch>` (the
probe is embedded in the boot image, so re-run mkboot + mkfs after
rebuilding userland).
"""
import subprocess
import sys
import threading
import time

ARCH = sys.argv[1] if len(sys.argv) > 1 else "x86"
MEM = sys.argv[2] if len(sys.argv) > 2 else "256M"

BASE = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-m", MEM, "-no-reboot",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]
RISCV = [
    "qemu-system-riscv64", "-machine", "virt", "-m", MEM, "-nographic",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0",
    "-kernel", "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64",
]
AARCH64 = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57",
    "-m", MEM, "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough",
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
    print("=== RESULT: FAIL (no prompt) ===")
    qemu.kill()
    sys.exit(1)
time.sleep(0.5)

qemu.stdin.write(b"allocprobe\n")
qemu.stdin.flush()
passed = wait_for(lambda: b"allocprobe: PASS" in bytes(out), 20)
failed = wait_for(lambda: b"allocprobe: FAIL" in bytes(out), 20)
# Drain tail output so the verdict line is complete before we print it.
time.sleep(2)

qemu.kill()
try:
    qemu.wait(timeout=5)
except subprocess.TimeoutExpired:
    pass

text = bytes(out).decode("ascii", "replace")
print(text)
verdict = "PASS" if passed and not failed else ("FAIL" if failed else "UNKNOWN (no verdict)")
print("=== RESULT: %s ===" % verdict)
sys.exit(0 if verdict == "PASS" else 1)
