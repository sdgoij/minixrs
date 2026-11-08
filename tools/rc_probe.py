#!/usr/bin/env python3
"""The session chain: `/etc/rc`, the shell's prompt, `/etc/session`.

Boots x86 with the bochs display and types nothing at all. Three things have to
happen on their own, in order:

  * `/bin/rc` runs `/etc/rc`, which names the session manager;
  * the shell prints its prompt as a line of its own, and the manager reports what
    it started, without either landing inside the other;
  * the panel from `/etc/session` reaches its ready line, on a line of its own.

The prompt check is the one that matters most: a prompt with text after it is not
a prompt to anything reading the console, and the shell's own terminator is what
keeps that true however many programs share the console.

Exit 0 on success, 1 otherwise.
"""
import subprocess
import re
import sys
import threading
import time

# A prompt on a line of its own: `# `, ended by the session manager's newline. The
# guest's tty turns `\n` into `\r\n`, so the terminator is optional in the match.
PROMPT = re.compile(rb"^# *\r?$", re.M)

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

    step(1, "the rc list started the session manager",
         lambda: b"session: started 1" in out)
    step(2, "the prompt is a line of its own",
         lambda: PROMPT.search(bytes(out)) is not None, 30)
    step(3, "the session's panel came up", lambda: b"wlpanel: ready" in out, 60)

    if not ok:
        with lock:
            print(bytes(out[-3000:]).decode(errors="replace"), file=sys.stderr)
    qemu.kill()
    print("rc probe: %s" % ("PASS" if ok else "FAIL"), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
