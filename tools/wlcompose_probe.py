#!/usr/bin/env python3
"""The 3d-b-5b gate: GPU composition across processes.

`/bin/wlcompose serve` (one process) holds an EGL context and the Mesa DSOs; `/bin/wlcompose test`
(another) makes two GL surfaces on the render node, hands their dma-buf descriptors over a
UNIX-domain socket with `SCM_RIGHTS`, and asks the helper to compose them into one frame. The
helper imports the peer's descriptors, draws them as quads, reads the frame back once
(`.notes/wayland.md` §6.10, D16, §5 row 27), and returns the frame's descriptor; the peer imports
that and checks the two rectangles and the background.

What this measures is **cross-process sampling** — whether a surface one process rendered on the
render node is visible to another process's context. The peer only reaches `PASS` if it read the
peer-drawn colours out of the returned frame, so a `FAIL` with the pixel values is the measurement,
not a crash.

Like the other Mesa gates this needs a GL-capable dev host (headless CI has no GL). The QEMU line
is the 4d gate's — a 2D `virtio-gpu-pci` first and a GL device second — because those are the
devices the port's role selection (D11) has been validated against.
"""
import subprocess
import sys
import threading
import time

lock = threading.Lock()
out = bytearray()
qemu = None


def pump():
    while True:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            break
        with lock:
            out.extend(chunk)


def send_paced(cmd, per_byte_ms=20):
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def type_line(cmd, tries=4):
    """Type `cmd` and do not accept it until the guest has echoed the whole of it.

    A GL display drops console bytes mid-line (the reason `feed.sh` has `FEED_PACE`), and a
    truncated line still *runs*, as a shorter command. So each attempt is paced, and checked by the
    full echo before it counts.
    """
    want = cmd.encode()
    for _ in range(tries):
        with lock:
            offset = len(out)
        send_paced(cmd)
        if wait_for(lambda: want in bytes(out[offset:]), 25):
            return True
        time.sleep(1.5)
    return False


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
    argv = sys.argv[1:]
    if not argv:
        print("usage: wlcompose_probe.py <qemu command...>", file=sys.stderr)
        sys.exit(2)
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    def die(msg):
        with lock:
            print("%s; tail:\n%s" % (msg, bytes(out[-2000:]).decode(errors="replace")),
                  file=sys.stderr)
        qemu.kill()
        sys.exit(1)

    if not wait_for(lambda: b"# " in out, 240):
        die("NO PROMPT")
    time.sleep(1.0)

    # The helper in the background, then the peer. The shell's `&` forks it; it prints `ready`
    # once it has bound, but the peer retries anyway, so no ordering wait is needed here.
    if not type_line("/bin/wlcompose serve &"):
        die("could not type /bin/wlcompose serve & — the console dropped bytes")
    time.sleep(1.5)
    if not type_line("/bin/wlcompose test"):
        die("could not type /bin/wlcompose test — the console dropped bytes")

    passed = wait_for(lambda: b"wlcompose: PASS" in out, 120)
    with lock:
        data = bytes(out)
    # A GL display drops console bytes mid-line, so the captured stream is also kept whole, for a
    # failed run to be read back without the terminal's mangling.
    with open("target/wlcompose.log", "wb") as f:
        f.write(data)
    for line in data.splitlines():
        if b"wlcompose:" in line or b"GL_RENDERER" in line:
            print("serial: %s" % line.decode(errors="replace"), flush=True)
    qemu.kill()
    if not passed:
        print("wlcompose: no PASS; the run is in target/wlcompose.log", file=sys.stderr)
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
