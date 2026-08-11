#!/usr/bin/env python3
"""Drive x86 QEMU: run N `hello` execs, sampling `memstat` every K execs.

No gdb stub / QMP (they corrupt piped serial input), so this is the clean
way to measure the repeated-exec leak: the kernel's free-page count as
reported by the memstat shell builtin.

Usage:
  python3 tools/exec_loop_mem.py [N] [MEM] [K]

The guest is deemed hung when an exec (or memstat) does not answer within
the wait window; the driver then reports the last known free count.
"""
import re
import subprocess
import sys
import threading
import time

N = int(sys.argv[1]) if len(sys.argv) > 1 else 100
MEM = sys.argv[2] if len(sys.argv) > 2 else "256M"
K = int(sys.argv[3]) if len(sys.argv) > 3 else 10
CMD = sys.argv[4] if len(sys.argv) > 4 else "hello"
MARKER = sys.argv[5] if len(sys.argv) > 5 else "pid="
COMPLETE = sys.argv[6] if len(sys.argv) > 6 else "threadstd"

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-m", MEM, "-no-reboot",
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
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


def wait_marker_after(base, marker, timeout):
    """Wait until `marker` appears in the output appended after `base`.

    Position-based so the driver never stale-matches a previous exec's
    output (the old last-N-bytes check let it run ahead of the guest and
    report a false wedge when memstat landed mid-exec).
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            cur = len(out)
            if marker in bytes(out[base:cur]):
                return True
        time.sleep(0.05)
    with lock:
        return marker in bytes(out[base:len(out)])


def send(cmd):
    """Write a command to the guest without ever blocking the driver (the
    write runs in a thread; a wedged guest must not freeze the probe)."""
    result = {}

    def do_write():
        try:
            qemu.stdin.write(cmd.encode() + b"\n")
            qemu.stdin.flush()
            result["ok"] = True
        except Exception as e:  # noqa: BLE001
            result["err"] = e

    t = threading.Thread(target=do_write, daemon=True)
    t.start()
    t.join(3)
    if "ok" not in result:
        return False
    return True


def run_memstat(tag):
    """Send `memstat`, wait for the NEXT 'mem: page ... free N' line (one
    that appears after the command was sent), return N."""
    with lock:
        start = len(out)
    if not send("memstat"):
        print("memstat[%s] WRITE BLOCKED" % tag, file=sys.stderr, flush=True)
        return -1
    deadline = time.time() + 8
    while time.time() < deadline:
        with lock:
            m = re.search(rb"mem: page \d+ total \d+ pages free (\d+)", bytes(out[start:]))
            if m:
                val = int(m.group(1))
                print("memstat[%s] free=%d pages (%.1f MiB)" % (tag, val, val * 4 / 1024),
                      file=sys.stderr, flush=True)
                return val
        time.sleep(0.05)
    print("memstat[%s] TIMEOUT" % tag, file=sys.stderr, flush=True)
    return -1


try:
    saw_prompt = wait_for(lambda: b"# " in out[-80:], 60)
    print("prompt: %s" % saw_prompt, file=sys.stderr, flush=True)
    if not saw_prompt:
        sys.exit(1)
    time.sleep(0.5)
    baseline = run_memstat("boot")
    points = [(0, baseline)]
    hung = None
    for i in range(N):
        if not send(CMD):
            hung = i
            print("HANG: exec %d could not be sent" % (i + 1), file=sys.stderr, flush=True)
            break
        with lock:
            base = len(out)
        if not wait_marker_after(base, MARKER.encode(), 12):
            hung = i
            print("HANG: exec %d did not complete" % (i + 1), file=sys.stderr, flush=True)
            send("hangdump")
            time.sleep(3)
            break
        if not wait_marker_after(base, COMPLETE.encode(), 10):
            hung = i
            print("HANG: exec %d no completion marker" % (i + 1), file=sys.stderr, flush=True)
            send("hangdump")
            time.sleep(3)
            break
        # Sync with the guest: the shell prints a bare `# ` prompt after
        # reaping the child (no newline — the typed command echoes on the
        # same line). Only treat it as this exec's prompt if it appears
        # after this exec's completion marker, so the driver never runs
        # ahead of the reap.
        if not wait_marker_after(base, b"# ", 10):
            hung = i
            print("HANG: no prompt after exec %d" % (i + 1), file=sys.stderr, flush=True)
            send("hangdump")
            time.sleep(3)
            break
        time.sleep(0.3)
        if (i + 1) % K == 0:
            f = run_memstat("exec %d" % (i + 1))
            if f < 0:
                # A memstat timeout is usually the write landing while the
                # guest is mid-output; retry once before declaring a hang.
                time.sleep(2)
                f = run_memstat("exec %d retry" % (i + 1))
            if f < 0:
                hung = i
                send("hangdump")
                time.sleep(3)
                break
            points.append((i + 1, f))

    if hung is not None:
        f = run_memstat("hang")
        if f < 0:
            # The shell is stuck in the memstat builtin; dump again to
            # catch it blocked in the sendrec (the first dump ran while the
            # shell was still executing the hangdump builtin).
            send("hangdump")
            time.sleep(3)
        if f >= 0:
            points.append((hung, f))

    print("=== points: %s ===" % points, file=sys.stderr, flush=True)
    if len(points) >= 2 and points[-1][1] >= 0:
        (e0, f0), (e1, f1) = points[0], points[-1]
        d = f0 - f1
        per = d / (e1 - e0) if e1 > e0 else 0
        print("=== RESULT: %d execs done, leak %d pages (%.1f MiB), %.1f KiB/exec ===" % (
            N if hung is None else hung, d, d * 4 / 1024, per * 4), file=sys.stderr, flush=True)
        ok = hung is None and per < 16  # < 64 KiB/exec
        print("=== VERDICT: %s ===" % ("PASS" if ok else "FAIL"), file=sys.stderr, flush=True)
finally:
    qemu.kill()
    try:
        qemu.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    with lock:
        text = bytes(out).decode("ascii", "replace")
    tail = [l for l in text.splitlines() if not l.startswith("  /sbin")]
    print("\n".join(tail[-60:]))
