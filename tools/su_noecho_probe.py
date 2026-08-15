#!/usr/bin/env python3
"""J4 echo-hide probe: su's password prompt must not echo the typed password.

Boots the x86 image, runs `su test`, sends `test123`, and asserts the
digits never appear on the serial console between the `Password: ` prompt
and the shell prompt that follows (a successful su also proves the
password was accepted). Exits with 0 on PASS.
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


def main():
    if not wait_for(has_prompt, 90):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.5)

    # su test -> password prompt.
    base = len(out)
    send("su test")
    if not wait_for(lambda b: b"Password:" in out[b:], 15, base):
        print("NO SU PROMPT: %r" % grab(base)[-300:], file=sys.stderr)
        sys.exit(1)
    mark = len(out)
    send("test123")
    if not wait_for(has_prompt, 15, mark):
        print("SU DID NOT EXEC SHELL: %r" % grab(mark)[-300:], file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)

    chunk = grab(mark)
    # `id` as the test user proves the switch happened and the password was
    # accepted; the password must not appear in the console output.
    b2 = len(out)
    send("id")
    if not wait_for(lambda b: b"groups=" in out[b:], 15, b2):
        print("NO ID OUTPUT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)
    m = re.search(rb"uid=[^\r\n]*groups=[^\r\n]*", grab(b2))
    id_line = m.group(0).decode() if m else ""

    fails = []
    if b"test123" in chunk:
        fails.append("password echoed on console")
    if "uid=1000" not in id_line:
        fails.append("uid switch failed: %r" % id_line)
    for f in fails:
        print("FAIL: " + f)
    print("RESULT: %s" % ("PASS" if not fails else "FAIL"))
    sys.exit(0 if not fails else 1)


if __name__ == "__main__":
    main()
