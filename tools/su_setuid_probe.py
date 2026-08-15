#!/usr/bin/env python3
"""J5 real-system probe: setuid-root exec on the x86 image.

Serial input is written one byte at a time (3 ms gap): QEMU's chardev
pushes pipe writes as bursts that overrun the 16550's 16-byte RX FIFO
(the x86 analog of KNOWN_ISSUES [riscv] #1), so unpaced commands lose
bytes. Pacing keeps the guest's ISR/timer drains ahead of the FIFO.

Flow:
1. Control — `/bin/sugid` as `test` (uid 1000) without the setuid bit:
   euid stays 1000, issetugid=0, /etc/secret denied.
2. Root `chmod 4755 /bin/sugid` (chmod can set the bit).
3. Setuid — `/bin/sugid` as test: euid becomes 0, issetugid=1,
   /etc/secret readable (permission gate passed via the elevated euid).
4. Round-trip — root `chown 1000:1000 /bin/sugid` clears the setuid bit
   (C/J2 behavior); the binary no longer elevates.
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
        time.sleep(0.02)
    with lock:
        return pred(base)


def has_prompt(base):
    return b"# " in out[base:]


def send_paced(cmd, per_byte_ms=3):
    """Write `cmd\\n` one byte at a time so the 16-byte UART FIFO never
    overruns (QEMU chardev bursts drop bytes on this port)."""
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


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


def run_as_test():
    """su test, run /bin/sugid, capture its line, exit back to root."""
    base = len(out)
    send_paced("su test")
    if not wait_for(lambda b: b"Password:" in out[b:], 15, base):
        print("NO SU PROMPT: %r" % grab(base)[-300:], file=sys.stderr)
        sys.exit(1)
    send_paced("test123")
    if not wait_for(has_prompt, 15, len(out)):
        print("SU DID NOT EXEC SHELL: %r" % grab(base)[-300:], file=sys.stderr)
        sys.exit(1)
    b2 = len(out)
    send_paced("/bin/sugid")
    if not wait_for(lambda b: b"secret=" in out[b:], 15, b2):
        print("NO SUGID OUTPUT: %r" % grab(b2)[-300:], file=sys.stderr)
        sys.exit(1)
    time.sleep(0.2)
    chunk = grab(b2)
    m = re.search(
        rb"ruid=(\d+) euid=(\d+) rgid=(\d+) egid=(\d+) issetugid=([01]) secret=([^\r\n]+)",
        chunk,
    )
    if not m:
        print("SUGID LINE UNPARSEABLE: %r" % chunk[-300:], file=sys.stderr)
        sys.exit(1)
    line = m.group(0).decode()
    vals = {"ruid": m.group(1).decode(), "euid": m.group(2).decode(),
            "rgid": m.group(3).decode(), "egid": m.group(4).decode(),
            "issetugid": m.group(5).decode(), "secret": m.group(6).decode()}
    send_paced("exit")
    if not wait_for(has_prompt, 15, len(out)):
        print("NO ROOT PROMPT AFTER EXIT", file=sys.stderr)
        sys.exit(1)
    return vals, line


def run_root(cmd):
    """Run a command as root; return (ok, chunk)."""
    b = len(out)
    send_paced(cmd)
    if not wait_for(has_prompt, 15, b):
        print("NO PROMPT after %r: %r" % (cmd, grab(b)[-300:]), file=sys.stderr)
        sys.exit(1)
    time.sleep(0.2)
    return grab(b)


def main():
    if not wait_for(has_prompt, 90):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(1.0)

    # 1. Control: no setuid bit yet (0755).
    v, line = run_as_test()
    check("control: euid stays 1000", v["euid"] == "1000", line)
    check("control: not tainted", v["issetugid"] == "0", line)
    check("control: /etc/secret denied", v["secret"] == "DENIED", line)

    # 2. Root sets the setuid bit; chmod must not error.
    chunk = run_root("chmod 4755 /bin/sugid")
    check("chmod 4755 sets the bit (no error)", b"chmod:" not in chunk, chunk[-200:].decode(errors="replace"))

    # 3. Setuid exec: euid elevates to 0, tainted, secret readable.
    v, line = run_as_test()
    check("setuid: euid elevated to 0", v["euid"] == "0" and v["ruid"] == "1000", line)
    check("setuid: issetugid=1", v["issetugid"] == "1", line)
    check("setuid: /etc/secret readable", v["secret"] == "top secret", line)

    # 4. Round-trip: chown clears the setuid bit (J2), no elevation.
    chunk = run_root("chown 1000:1000 /bin/sugid")
    check("chown ok (no error)", b"chown:" not in chunk, chunk[-200:].decode(errors="replace"))
    v, line = run_as_test()
    check("chown cleared setuid: euid back to 1000", v["euid"] == "1000", line)
    check("chown cleared setuid: not tainted", v["issetugid"] == "0", line)

    print("RESULT: %s" % ("PASS" if not fails else "FAIL: %s" % fails))
    sys.exit(0 if not fails else 1)


if __name__ == "__main__":
    main()
