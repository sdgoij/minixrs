#!/usr/bin/env python3
"""The cosmic-comp bring-up gate: the compositor boots, binds its socket, presents a frame.

    python tools/cosmic_probe.py [--client] <qemu command...>

With `--client`, the compositor is started with `/bin/wlshm` as its kiosk child instead, so a
stock `wayland-client` has to reach it, commit two `wl_shm` frames and get the first buffer
back. That is the end-to-end proof that the socket, the seat and the renderer serve a client,
not just the scan-out.

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
    """Wait until `pred` is true, or the timeout expires.

    `pred` is called with the buffer's lock held, so it must not take the lock
    itself: `lock` is a plain `Lock`, and re-entering it deadlocks the probe (which
    is how a run once hung until its own kill timeout).
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.05)
    with lock:
        return pred()


def offset():
    with lock:
        return len(out)


def seen_since(marker, start):
    """Lock-free on purpose: called from a `wait_for` predicate, which holds the lock."""
    return marker in out[start:]


def type_command(cmd, attempts=5):
    """Type `cmd` and retype it until the shell actually ran it.

    This console drops bytes, and two separate symptoms need covering: a truncated
    line echoes short, and (measured) a whole line can be echoed while the shell
    still runs a name with a byte missing, which it reports as `not found`. The
    echo plus the absence of that report is the evidence; both are per attempt, so
    a line the guest really did receive is not typed twice.
    """
    for _ in range(attempts):
        mark = offset()
        send_paced(cmd)
        wait_for(lambda: seen_since(cmd.encode(), mark), 15)
        time.sleep(1.5)
        with lock:
            window = bytes(out[mark:])
        if cmd.encode() in window and b"not found" not in window:
            return True
        # Let whatever ran finish (it prints an error and a new prompt) before
        # typing again.
        wait_for(lambda: out[mark:].count(b"\n") >= 2, 15)
    return False


def tail(n=1600):
    with lock:
        return bytes(out[-n:]).decode("utf-8", "replace")


def main(argv):
    global qemu
    if not argv:
        print("usage: cosmic_probe.py [--client] <qemu command...>", file=sys.stderr)
        return 2

    client = "--client" in argv
    argv = [arg for arg in argv if arg != "--client"]
    guest_command = "/bin/cosmic-comp /bin/wlshm" if client else "/bin/cosmic-comp"

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
        typed = type_command(guest_command)
        check(typed, "the compositor's command reached the shell intact", "" if typed else tail())
        if not typed:
            return 1

        framed = wait_for(lambda: FRAME_MARKER in out, FRAME_TIMEOUT)
        check(
            framed,
            "the compositor bound its socket and put its first frame on the scan-out",
            "" if framed else tail(),
        )
        if not framed:
            return 1

        if client:
            passed = wait_for(lambda: b"wlshm: PASS" in out, 120)
            check(
                passed,
                "a stock wl_shm client connected, committed and got its buffer back",
                "" if passed else tail(),
            )

        # A compositor that died after presenting would pass the check above.
        time.sleep(QUIESCENCE)
        with lock:
            text = bytes(out)
        for marker in (b"panicked", b"render failed", b"Presentation failed", b"G0000"):
            if marker in text:
                check(False, "nothing crashed", marker.decode())
                break
        else:
            if client:
                check(True, "nothing crashed, and the client's frames reached the compositor")
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
