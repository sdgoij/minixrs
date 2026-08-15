#!/usr/bin/env python3
"""Boot the x86 image, run `id` at the shell prompt, and print its output.

One-shot driver: waits for the `# ` prompt, sends `id`, waits for the
`groups=` completion marker, dumps everything after the marker line, and
exits 0 when the output matches the expected uid/gid shape.
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


def wait_for(pred, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


def send(cmd):
    qemu.stdin.write(cmd.encode() + b"\n")
    qemu.stdin.flush()


def main():
    if not wait_for(lambda: b"# " in out[-80:], 90):
        print("NO PROMPT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.5)
    with lock:
        base = len(out)
    send("id")
    if not wait_for(lambda: b"groups=" in out[base:], 15):
        print("NO ID OUTPUT", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.3)
    with lock:
        chunk = bytes(out[base:])
    # The output spans from the typed command echo to the next prompt.
    m = re.search(rb"uid=[^\r\n]*groups=[^\r\n]*", chunk)
    if not m:
        print("UNPARSEABLE: %r" % chunk[-400:], file=sys.stderr)
        sys.exit(1)
    line = m.group(0).decode()
    print("ID_LINE: %s" % line)
    # Root shell: uid=0 euid=0 gid=0 egid=0 groups=
    ok = re.match(r"^uid=0 euid=0 gid=0 egid=0 groups=$", line) is not None
    print("MATCH: %s" % ok)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
