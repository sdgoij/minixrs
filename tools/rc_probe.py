#!/usr/bin/env python3
"""The rc step: `/etc/rc` runs at boot, before anything is typed.

Boots x86 with the bochs display and, without sending a single command, waits for
the line `/etc/rc` names. `/bin/rc` is what init execs: it reads the file, starts
each line in the background, and then becomes the shell, so the line arriving on
its own proves the whole chain (init, the list, the fork, the program).

Exit 0 on success, 1 otherwise.
"""
import subprocess
import sys
import threading
import time

QMP_PORT = 4476

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none", "-display", "none",
    "-m", "256M", "-no-reboot", "-vga", "none",
    "-device", "bochs-display,id=fb0",
    "-qmp", "tcp:127.0.0.1:%d,server=on,wait=off" % QMP_PORT,
    "-kernel", "target/images/x86_64-pc-minix/minix-x86.elf",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
    "-device", "virtio-tablet-pci,display=fb0",
]

lock = threading.Lock()
out = bytearray()


def pump():
    while True:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            break
        with lock:
            out.extend(chunk)


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
    qemu = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    threading.Thread(target=pump, daemon=True).start()

    ok = True

    def step(n, name, pred, timeout=150):
        nonlocal ok
        if not ok:
            return
        if wait_for(pred, timeout):
            print("[%d] %s: ok" % (n, name), flush=True)
        else:
            print("[%d] %s: FAIL" % (n, name), flush=True)
            ok = False

    step(1, "the rc list ran at boot", lambda: b"rc: session" in out)
    step(2, "the shell came up after it", lambda: b"# " in out, 30)

    if not ok:
        with lock:
            print(bytes(out[-3000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("rc probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
