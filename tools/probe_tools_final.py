#!/usr/bin/env python3
"""Final verification of the kept feat_minix tools (unlink, fold, basenc,
shred) on a fresh disk. env/ptx were removed (x86 exec bug). Uses one pipe
only (the second pipeline wedges the x86 shell — pre-existing bug)."""
import subprocess
import sys
import threading
import time


def main():
    port = 4444
    qemu = subprocess.Popen(
        ["qemu-system-x86_64", "-nographic", "-monitor", "none",
         "-m", "256M", "-no-reboot",
         "-qmp", "tcp:127.0.0.1:%d,server=on,wait=off" % port,
         "-kernel", "target/trampoline.elf",
         "-device", "loader,file=target/kernel.bin,addr=0x200000",
         "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
         "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    out = bytearray()
    threading.Thread(target=lambda: _rd(qemu, out), daemon=True).start()

    def waitfor(m, t):
        b = len(out)
        dl = time.time() + t
        while time.time() < dl:
            if m in bytes(out[b:]):
                return True
            time.sleep(0.05)
        return False

    def send(cmd, t=8):
        base = len(out)
        if isinstance(cmd, str):
            cmd = cmd.encode()
        for ch in cmd + b"\n":
            qemu.stdin.write(bytes([ch]))
            qemu.stdin.flush()
            time.sleep(0.02)
        ok = waitfor(b"# ", t)
        return ok, bytes(out[base:])

    fails = []
    try:
        ok = waitfor(b"# ", 180)
        print("boot:", ok, flush=True)
        if not ok:
            return 1
        time.sleep(1.0)

        # Setup files.
        _, c = send("echo hello > /tmp/h")
        _, c = send("cat /tmp/h")
        print("setup h:", repr(c[-40:]), flush=True)
        _, c = send("echo abcdefghij > /tmp/f")
        _, c = send("echo x > /tmp/u")
        _, c = send("echo data > /tmp/s")

        # fold on a FILE and on a PIPE (first pipe of the session).
        _, c = send("coreutils fold -w 5 /tmp/f")
        print("fold file:", repr(c[-40:]), flush=True)
        if b"abcde" not in c or b"fghij" not in c:
            fails.append("fold file: bad")
        _, c = send("echo abcdefghij | coreutils fold -w 5")
        print("fold pipe:", repr(c[-40:]), flush=True)
        if b"abcde" not in c or b"fghij" not in c:
            fails.append("fold pipe: bad")

        # basenc --base64 on a FILE.
        _, c = send("coreutils basenc --base64 /tmp/h")
        print("basenc --base64 file:", repr(c[-40:]), flush=True)
        if b"aGVsbG8K" not in c:
            fails.append("basenc file: wrong")

        # shred -f -u on a file, verify gone.
        _, c = send("coreutils shred -f -u /tmp/s")
        print("shred:", repr(c[-40:]), flush=True)
        _, c = send("cat /tmp/s")
        print("cat after shred:", repr(c[-40:]), flush=True)
        if b"cannot open" not in c:
            fails.append("shred: not removed")

        # unlink on a fresh file, verify gone.
        _, c = send("coreutils unlink /tmp/u")
        print("unlink:", repr(c[-40:]), flush=True)
        _, c = send("cat /tmp/u")
        print("cat after unlink:", repr(c[-40:]), flush=True)
        if b"cannot open" not in c:
            fails.append("unlink: not removed")

        whole = bytes(out)
        for marker, name in [(b"^@", "NUL flood"), (b"\nG", "#GP"), (b"\nU", "ud2")]:
            n = whole.count(marker)
            print("%s count: %d" % (name, n), flush=True)
            if n:
                fails.append("marker %s x%d" % (name, n))
    finally:
        qemu.kill()
        try:
            qemu.wait(timeout=5)
        except Exception:
            pass
    print("FAILURES:", fails if fails else "none", flush=True)
    return 1 if fails else 0


def _rd(proc, out):
    while True:
        c = proc.stdout.read1(4096)
        if not c:
            break
        out.extend(c)


if __name__ == "__main__":
    sys.exit(main())
