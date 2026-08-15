#!/usr/bin/env python3
"""J4 real-system probe: su -> id (uid switch) and /etc/secret permission
enforcement on the x86 image.

Drives the serial console: boots, waits for the `# ` prompt, runs
`su test` with password `test123`, checks `id` reports uid=1000, checks
`cat /etc/secret` is denied for the test user, then exits back to the
root shell and checks root can read it.
"""
import re
import subprocess
import sys
import threading
import time

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
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


def has_prompt(base):
    return b"# " in out[base:]


def send(cmd):
    qemu.stdin.write(cmd.encode() + b"\n")
    qemu.stdin.flush()


def grab(base):
    with lock:
        return bytes(out[base:])


fails = []


def check(name, cond, detail=""):
    print("%s: %s" % ("OK" if cond else "FAIL", name))
    if not cond:
        fails.append(name)
        if detail:
            print("  " + detail)


def main():
    if not wait_for(has_prompt, 90):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.5)

    # 1. su test -> password prompt -> test123.
    base = len(out)
    send("su test")
    if not wait_for(lambda b: b"Password:" in out[b:], 15, base):
        print("NO SU PROMPT: %r" % grab(base)[-300:], file=sys.stderr)
        sys.exit(1)
    send("test123")
    if not wait_for(has_prompt, 15, len(out)):
        print("SU DID NOT EXEC SHELL: %r" % grab(base)[-300:], file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)

    # 2. id as the test user.
    b2 = len(out)
    send("id")
    if not wait_for(lambda b: b"groups=" in out[b:], 15, b2):
        print("NO ID OUTPUT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)
    chunk = grab(b2)
    m = re.search(rb"uid=[^\r\n]*groups=[^\r\n]*", chunk)
    id_line = m.group(0).decode() if m else ""
    check("su test -> id shows uid=1000", "uid=1000" in id_line and "gid=1000" in id_line, id_line)

    # 3. cat /etc/secret as the test user must be denied.
    b3 = len(out)
    send("cat /etc/secret")
    time.sleep(1.5)
    chunk = grab(b3)
    denied = b"cannot open" in chunk or b"denied" in chunk or b"ermission" in chunk
    check("test user denied /etc/secret", denied, chunk.decode(errors="replace")[-200:])

    # 4. exit back to the root shell; root can read /etc/secret.
    b4 = len(out)
    send("exit")
    if not wait_for(has_prompt, 15, b4):
        print("NO ROOT PROMPT AFTER EXIT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)
    b5 = len(out)
    send("cat /etc/secret")
    if not wait_for(lambda b: b"top secret" in out[b:], 15, b5):
        print("ROOT CAT FAILED: %r" % grab(b5)[-300:], file=sys.stderr)
        sys.exit(1)
    check("root reads /etc/secret", True)

    print("RESULT: %s" % ("PASS" if not fails else "FAIL: %s" % fails))
    sys.exit(0 if not fails else 1)


if __name__ == "__main__":
    main()
