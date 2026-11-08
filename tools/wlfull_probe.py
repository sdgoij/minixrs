#!/usr/bin/env python3
"""The 3d-b gate: a whole-output `wl_shm` frame presented by re-pointing the scanout.

`/bin/wlclient full` draws a 1024x768 pool and commits it at the output's own size. `/sbin/wlserver`
hands that buffer to the fb server (`FBIO_SCANOUT_SRC`) instead of blitting it, so the virtio-gpu
scanout becomes the client's memory and the compositor copies nothing (`.notes/wayland.md` 6.10,
3d-b). The frame is then *the pool*, so `/dev/fb` (the server's own buffer) no longer holds it;
this probe checks the display from the host instead: a QMP screendump must be the client's colour,
and the serial log must carry the fb server's `fb: scanout src` line proving the path was taken.

The QEMU line uses `virtio-gpu-pci` as the display, with no `bochs-display`: the re-point is a
virtio-gpu capability, so the output device has to *be* the virtio-gpu one (D11).

The session starts `/bin/wlpanel` before anything is typed, and a panel is a second surface:
with two in the scene the server composes the frame into its own buffer instead of handing the
client's buffer to the scanout, which is the path this gate exists to measure. So the probe
ends the panel first, by the pid its ready line reports (moving a pointer onto it would need a
pointer device, and riscv64/aarch64 are booted with none), and goes on once the shell is back.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

SCREEN = "target/wlfull.ppm"

# The client runs at the output's own size: whole-output, so the server can scan the pool out.
CMD = "/bin/wlclient full"

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


def qmp_cmd(s, obj):
    s.sendall((json.dumps(obj) + "\n").encode())
    data = b""
    while True:
        chunk = s.recv(65536)
        data += chunk
        try:
            return json.loads(data.decode().splitlines()[-1])
        except Exception:
            if len(data) > (1 << 20):
                return None


def send_paced(cmd, per_byte_ms=20):
    """Type `cmd` a byte at a time, slowly enough for this guest's console.

    Twenty milliseconds is `tools/smoke/feed.sh`'s `FEED_PACE`, the pace measured on a guest
    whose console mangles bursts. It is the default here because the arches this gate runs on
    beyond x86 are like that, and a mangled line is not a failed send: it still runs.
    """
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


def key(s, name, down):
    """One key press, through whichever input device QEMU routes it to.

    No `device` is named: the guest reads virtio-input devices, and on every arch this gate
    runs on one is present (the tablet on x86, a keyboard on riscv64/aarch64), which is what
    makes the key reach the guest's input server at all.
    """
    return qmp_cmd(s, {"execute": "input-send-event",
                       "arguments": {"events": [
                           {"type": "key",
                            "data": {"key": {"type": "qcode", "data": name}, "down": down}}
                       ]}})


def run_line(cmd, timeout=15):
    """Type one shell command and wait for the shell to come back to its prompt.

    The prompt is printed before the bytes are read, so the echo contains `# ` too; the wait
    starts from the echoed command, and the prompt after *that* is the shell's next one.
    """
    with lock:
        sent = len(out)
    send_paced(cmd)
    if not wait_for(lambda: cmd.encode() in out[sent:], timeout):
        return False
    with lock:
        at = bytes(out).rfind(cmd.encode())
    return wait_for(lambda: b"# " in out[at:], timeout)


def type_line_and_wait(cmd, want, timeout, attempts=4):
    """Type `cmd` at the prompt and wait for `want`, retyping a line the guest never echoed.

    The echo is what says whether the line arrived, because this console mangles lines: CI
    failed this gate with `sh: '/bin/wclient' not found`, a path nobody typed, and a local
    run delivered `/bin/wlclient full` as `b/in/wllcient full`, with two pairs transposed. So
    "the client did not print PASS" alone cannot tell a client failure from a bad send. A line
    the guest never echoed whole is typed again; one that *was* echoed and did not pass is the
    client's own failure, and is reported rather than retried.

    The echo check reads from one anchor taken before the first attempt, as `tools/smoke/feed.sh`
    does, so a send that arrives late still counts and is not typed twice. What is anchored per
    attempt is the prompt: the shell prints it before the bytes, so a prompt seen before this
    attempt was typed says nothing about where this one has got to. Without that split a retry
    reads the previous attempt's answer, sees its own echo, and reports a client failure before
    the retyped line has run at all.
    """
    with lock:
        anchor = len(out)
    for _ in range(attempts):
        with lock:
            sent = len(out)
        send_paced(cmd)
        # Either the client answered, or the shell came back after running what it received.
        # The prompt only follows the command's completion, so this returns on a mangled line
        # too instead of spending the whole budget on it.
        wait_for(lambda: want in out[anchor:] or b"# " in out[sent:], timeout)
        with lock:
            seen = bytes(out[anchor:])
        if want in seen:
            return True
        if cmd.encode() in seen:
            return False
        # The line arrived truncated or not at all, so the shell printed the path it got and
        # came back to a prompt. Type again after that answer rather than into a running one.
        wait_for(lambda: b"# " in out[sent:], 5)
    return False


def read_screendump():
    """Read `SCREEN` once QEMU has finished writing it.

    QEMU answers `screendump` *before* the file is complete, so the fixed sleep this used to
    wait with is a guess a loaded host loses: the read then sees a partial file, or EAGAIN.
    The PPM header carries the size the file will be, so the wait is on that number and the
    answer is the frame, not a truncated one.
    """
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            with open(SCREEN, "rb") as f:
                data = f.read()
        except OSError:
            data = b""
        parts = data.split(b"\n", 3)
        if len(parts) == 4 and parts[0] == b"P6":
            dims = parts[1].split()
            if len(dims) == 2:
                w, h = int(dims[0]), int(dims[1])
                if len(parts[3]) >= w * h * 3:
                    return w, h, parts[3]
        time.sleep(0.05)
    return None


def screendump(s):
    # A complete file from an earlier dump must not answer for this one: QEMU overwrites the
    # path in place, so a read that beats the overwrite would report the previous frame.
    try:
        os.unlink(SCREEN)
    except OSError:
        pass
    r = qmp_cmd(s, {"execute": "screendump", "arguments": {"filename": SCREEN}})
    if r is None or r.get("return") is None:
        return None
    return read_screendump()


def pxat(screen, x, y):
    w, h, px = screen
    off = (y * w + x) * 3
    return px[off:off + 3]


def main():
    global qemu
    argv = sys.argv[1:]
    if not argv:
        print("usage: wlfull_probe.py <qemu command...>", file=sys.stderr)
        sys.exit(2)
    qemu = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"# " in out, 240):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(1.0)

    q = socket.create_connection(("127.0.0.1", 4447), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    # The session's panel is a second surface, and two surfaces is what takes the composed
    # path instead of the scanout, which is the path this gate exists to measure. The panel is
    # ended the way anything the session started can be, by naming the process it reported:
    # moving the pointer onto it would need a pointer device, and riscv64/aarch64 have none.
    if not wait_for(lambda: b"wlpanel: ready pid=" in out, 60):
        print("NO PANEL", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    with lock:
        banner = bytes(out)
    at = banner.rfind(b"wlpanel: ready pid=")
    fields = banner[at:].split(b"\n", 1)[0]
    try:
        panel_pid = int(fields.split(b"=", 1)[1])
    except (IndexError, ValueError):
        panel_pid = 0
    if panel_pid <= 0:
        print("THE PANEL NAMED NO PID", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    if not run_line("kill %d" % panel_pid):
        print("THE PANEL COULD NOT BE KILLED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    # The server drops the surface when the connection closes, which it notices on its next
    # poll; the client below is what proves whether it did (the scanout line, or its absence).
    time.sleep(1.0)

    # Baseline: the display is live and does not hold the client's frame yet. The fb server's
    # boot pattern used to be the marker, but the panel paints inside it (and its strip is
    # still on screen after it leaves, since nothing repaints), so what is asserted here is
    # the property the final check needs: the magenta below is the client's, not something
    # that was already up.
    magenta = bytes([255, 0, 255])
    base = screendump(q)
    if base is None:
        print("BASELINE SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    base_ok = pxat(base, 512, 384) != magenta and pxat(base, 0, 0) != magenta
    print("baseline centre %s corner %s" % (pxat(base, 512, 384), pxat(base, 0, 0)), flush=True)
    if not base_ok:
        print("BASELINE RGB MISMATCH", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)

    # A whole-output client: its buffer is the output's size, so the server can scan it out.
    if not type_line_and_wait(CMD, b"wlclient: PASS", 40):
        with lock:
            print("wlclient full did not reach PASS; tail:\n%s"
                  % bytes(out[-1000:]).decode(errors="replace"), file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    took_path = (b"fb: scanout src" in out) and (b"fb: scanout src refused" not in out)
    with lock:
        for line in bytes(out).splitlines():
            if b"fb: scanout" in line:
                print("serial: %s" % line.decode(errors="replace"), flush=True)
    print("fb took the scanout path: %s" % took_path, flush=True)

    time.sleep(0.5)
    shot = screendump(q)
    if shot is None:
        print("FINAL SCREENDUMP FAILED", file=sys.stderr)
        q.close()
        qemu.kill()
        sys.exit(1)
    # The pool is magenta: 0x00FF00FF little-endian is B=FF,G=00,R=FF -> RGB (255,0,255).
    centre = pxat(shot, 512, 384) == magenta
    corner = pxat(shot, 0, 0) == magenta
    print("after commit: centre magenta=%s, corner magenta=%s" % (centre, corner), flush=True)

    q.close()
    qemu.kill()
    sys.exit(0 if (took_path and centre and corner) else 1)


if __name__ == "__main__":
    main()
