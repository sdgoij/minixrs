#!/usr/bin/env python3
"""H3 piped-input parity matrix: spaced `hello` on one arch, one mode.

Boots the guest for the given arch/mode, waits for the shell prompt
(so input is never fed before the shell is reading — the H2
boot-timing-artifact trap), feeds 10 `hello` commands spaced ~1 s apart,
and counts "hello from minix std" lines in the output. Spacing avoids
the QEMU chardev burst artifact (the 16550's 16-byte RX FIFO overruns
when the whole pipe chunk is dumped at once — KNOWN_ISSUES [x86] and
[riscv] #1), so the test measures steady-state piped-input delivery,
not burst tolerance.

Usage:
  piped_matrix.py <arch> <mode> [runs]
    arch: x86 | riscv64 | aarch64
    mode: plain | s | qmp
    runs: number of boots (default 1); every boot must be 10/10 to pass

Exit 0 when every run delivers all 10 hellos; 1 otherwise.
"""
import subprocess
import sys
import threading
import time

QEMU = {
    "x86": [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-m", "256M", "-no-reboot", "-vga", "none",
        "-device", "bochs-display,id=fb0",
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ],
    "riscv64": [
        "qemu-system-riscv64", "-machine", "virt", "-m", "256M", "-nographic",
        "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-device", "virtio-gpu-device", "-device", "virtio-keyboard-device",
        "-kernel", "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64",
    ],
    "aarch64": [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57",
        "-m", "256M", "-nographic", "-no-reboot",
        "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-device", "virtio-gpu-device", "-device", "virtio-keyboard-device",
        "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
    ],
}

MODE_ARGS = {
    "plain": [],
    "s": ["-s"],
    "qmp": ["-qmp", "tcp:127.0.0.1:4444,server,nowait"],
}

N_HELLOS = 10
PROMPT_WAIT = 90  # seconds to wait for the shell prompt
HELLO_GAP = 1.0  # seconds between hello commands


def run_boot(arch, mode):
    """Boot the guest, feed spaced hellos, return the delivered count."""
    qemu = subprocess.Popen(
        QEMU[arch] + MODE_ARGS[mode],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
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

    def has_prompt():
        return b"# " in out

    def wait_for(pred, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            with lock:
                if pred():
                    return True
            time.sleep(0.02)
        with lock:
            return pred()

    if not wait_for(has_prompt, PROMPT_WAIT):
        qemu.kill()
        qemu.wait()
        with lock:
            sys.stderr.write("piped_matrix: no shell prompt (boot too slow?)\n")
            sys.stderr.write("  tail: %r\n" % bytes(out[-300:]))
        return -1

    # The prompt is up; give the shell's input path a moment, then feed the
    # spaced hellos.
    time.sleep(0.5)
    for _ in range(N_HELLOS):
        qemu.stdin.write(b"hello\n")
        qemu.stdin.flush()
        time.sleep(HELLO_GAP)

    # Drain the tail output, then stop the guest.
    time.sleep(3)
    try:
        qemu.stdin.close()
    except OSError:
        pass
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        qemu.kill()
        qemu.wait()

    with lock:
        return out.count(b"hello from minix std")


def main():
    if len(sys.argv) < 3:
        sys.stderr.write(__doc__)
        return 2
    arch = sys.argv[1]
    mode = sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    if arch not in QEMU:
        sys.stderr.write("piped_matrix: unknown arch %r (x86|riscv64|aarch64)\n" % arch)
        return 2
    if mode not in MODE_ARGS:
        sys.stderr.write("piped_matrix: unknown mode %r (plain|s|qmp)\n" % mode)
        return 2

    results = []
    for i in range(runs):
        count = run_boot(arch, mode)
        results.append(count)
        print("%s %s run %d/%d: %d/%d hellos"
              % (arch, mode, i + 1, runs, count, N_HELLOS))

    if any(c != N_HELLOS for c in results):
        print("RESULT: FAIL %s" % results)
        return 1
    print("RESULT: PASS (%d/%d hellos)" % (N_HELLOS, N_HELLOS))
    return 0


if __name__ == "__main__":
    sys.exit(main())
