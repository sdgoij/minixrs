#!/usr/bin/env python3
"""Drive an *interactive* bash over a QEMU guest's serial console.

    python tools/smoke/interactive_bash.py <log> <seconds> -- <qemu command…>

`tools/smoke/feed.sh` types into the minix shell and waits for its `#` prompt, so it
cannot reach an interactive bash (whose `bash-5.3#` prompt replaces it, and whose
readline polls the terminal instead of blocking on a line read). This driver waits
for each prompt and proves the part a `bash -c` scenario cannot: that readline's own
terminal setup and its poll-then-read path accept a typed line.

The marker is searched for at the start of a line, so the *echo* of the command
(`bash-5.3# echo _IBASH_OK_`) does not satisfy it — only the command's output does.
"""

from __future__ import annotations

import os
import subprocess
import sys
import threading
import time

MARKER = "_IBASH_OK_"


def main(argv: list[str]) -> int:
    if len(argv) < 4 or argv[2] != "--":
        print("usage: interactive_bash.py <log> <seconds> -- <qemu command…>", file=sys.stderr)
        return 2
    log_path, seconds, cmd = argv[0], float(argv[1]), argv[3:]

    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT)
    buf = bytearray()
    stop = threading.Event()

    def reader() -> None:
        # `os.read` is one `read(2)`: it returns as soon as a byte is available.
        # `proc.stdout.read(n)` would block for a full `n` bytes, so a prompt
        # shorter than the chunk would never be seen until the guest ended.
        fd = proc.stdout.fileno()
        while not stop.is_set():
            try:
                chunk = os.read(fd, 256)
            except OSError:
                break
            if not chunk:
                break
            buf.extend(chunk)

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()

    def wait_for(needle: bytes, timeout: float) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if needle in buf:
                return True
            time.sleep(0.05)
        return False

    def send(text: str) -> None:
        try:
            proc.stdin.write(text.encode())
            proc.stdin.flush()
        except (BrokenPipeError, OSError):
            pass

    ok = False
    reason = ""
    try:
        if not wait_for(b"# ", seconds):
            reason = "the guest never reached a shell prompt"
        else:
            send("bash\n")
            if not wait_for(b"bash-5.3#", seconds):
                reason = "bash never printed its prompt"
            else:
                send(f"echo {MARKER}\n")
                # At the start of a line: the command's echo carries a prompt in front of it.
                if wait_for(b"\n" + MARKER.encode(), seconds):
                    ok = True
                else:
                    reason = "bash ignored the typed line"
                send("exit\n")
    finally:
        time.sleep(0.2)
        stop.set()
        proc.kill()

    with open(log_path, "wb") as handle:
        handle.write(bytes(buf))

    if ok:
        print(f"interactive bash ran a typed line ({' '.join(cmd[0:1])})")
        return 0
    print(f"!! interactive bash: {reason}. Tail of {log_path}:", file=sys.stderr)
    sys.stderr.write(bytes(buf[-400:]).decode("utf-8", "replace"))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
