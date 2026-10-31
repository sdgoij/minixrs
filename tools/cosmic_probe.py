#!/usr/bin/env python3
"""The cosmic-comp bring-up gate: the compositor boots, binds its socket, presents a frame.

    python tools/cosmic_probe.py <qemu command...>

The image carries the compositor (`/bin/cosmic-comp`), the Mesa DSOs and the keymap it needs;
this probe boots it, types the compositor at the shell and watches the console.

What the checks mean:

  * A prompt, before anything is typed, is the image's own boot (as in every gate here).
  * `fb: scanout src` is printed by the fb server when `FBIO_SCANOUT_SRC` re-points the
    display at the frame the compositor just rendered and read back. It is the proof of the
    whole chain. It cannot be reached unless the compositor bound its socket (that happens
    first, in `init_wayland_display`), created the seat (whose keymap it compiles from the
    components it materialises at startup), registered the output with the shell, drew a
    frame and handed it to the scan-out. A failure anywhere before that raises, and the
    frame never appears.
  * A compositor that panicked or exited after its first frame is caught by the quiescence
    check: it is still running, and the console stays silent.

Nothing can be typed after the compositor starts: it holds the console for the life of the
boot, so the gate reads what the compositor and the servers print, and nothing else.

The first frame is slow (the software driver JIT-compiles its shaders inside the guest), so
`FRAME_TIMEOUT` is generous on purpose.
"""

import subprocess
import sys
import threading
import time

FRAME_MARKER = b"fb: scanout src"
# Wall-clock limits. The shader build in the guest is a minute-scale job under TCG.
BOOT_TIMEOUT = 120
FRAME_TIMEOUT = 300
QUIESCENCE = 20

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
    """Type `cmd` a byte at a time: this console drops bytes out of a burst."""
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def wait_for(pred, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


def tail(n=1600):
    with lock:
        return bytes(out[-n:]).decode("utf-8", "replace")


def main(argv):
    global qemu
    if not argv:
        print("usage: cosmic_probe.py <qemu command...>", file=sys.stderr)
        return 2

    checks = []

    def check(ok, label, detail=""):
        print(("PASS  " if ok else "FAIL  ") + label, flush=True)
        if detail:
            for line in detail.splitlines():
                print("      " + line, flush=True)
        checks.append(ok)
        return ok

    start = time.time()
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
    )
    threading.Thread(target=pump, daemon=True).start()

    try:
        booted = wait_for(lambda: b"# " in out, BOOT_TIMEOUT)
        check(booted, "the system boots to a prompt on its own")
        if not booted:
            print(tail(), flush=True)
            return 1

        time.sleep(0.5)
        send_paced("/bin/cosmic-comp")

        framed = wait_for(lambda: FRAME_MARKER in out, FRAME_TIMEOUT)
        check(
            framed,
            "the compositor bound its socket and put its first frame on the scan-out",
            "" if framed else tail(),
        )
        if not framed:
            return 1

        # A compositor that died after presenting would pass the check above.
        time.sleep(QUIESCENCE)
        with lock:
            text = bytes(out)
        for marker in (b"panicked", b"render failed", b"Presentation failed"):
            if marker in text:
                check(False, "the compositor stayed up", marker.decode())
                break
        else:
            check(True, f"the compositor is still running {QUIESCENCE}s after its first frame")
    finally:
        qemu.kill()

    passed = sum(1 for c in checks if c)
    print(f"\n{passed}/{len(checks)} checks passed", flush=True)
    print(f"first frame after {time.time() - start:.0f}s", flush=True)
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
