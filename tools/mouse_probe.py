#!/usr/bin/env python3
"""Phase N probe: mouse-driven window features on the wserver desktop.

Boots x86 with QMP + bochs-display, launches `/bin/wdemo info` (a 320x200
window at (40,40)), then drives the virtio mouse with QMP `input-send-event`
(targeting the bochs-display console via its `fb0` id, which the mouse is
bound to with `display=fb0`) and asserts via screendump:

1. The pointer renders (white arrow at the screen center on the desktop).
2. Moving the pointer to the info title bar moves the cursor pixels.
3. A title-bar drag moves the window (old title pixel becomes desktop,
   the new title shows focused blue).
4. Clicking the title-bar close button closes the window.
5. `/bin/wdemo ptr` (opt-in WS_PTR client) draws a '+' marker at a body
   click — the pointer-event round trip works end to end.

The mouse moves in small steps with a pause so the guest's alarm-poll
(50 ms) drains each batch before the next screendump.

This QEMU build (v11.0.0-12631) never delivers PS/2-mouse events, so the
probe (and `just desktop-x86`) use a virtio-tablet-pci bound to the
bochs-display console (`id=fb0`) — an absolute pointing device, so the
guest cursor tracks the host cursor 1:1 regardless of the SDL grab
state. QMP `input-send-event` with `"device": "fb0"` reaches the same
handler the SDL window feeds. QEMU resolves the device name by device
id, hence the `id=fb0` on the display.

Usage: python tools/mouse_probe.py
"""
import json
import socket
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display,id=fb0",
    "-m", "256M", "-no-reboot",
    "-qmp", "tcp:127.0.0.1:4460,server=on,wait=off",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    "-device", "virtio-tablet-pci,display=fb0",
]

SCREEN = "tools/mouse_screen.ppm"

# wdemo window geometry: create(40, 40, 320, 200).
WIN_X = 40
WIN_Y = 40
WIN_W = 320
WIN_H = 200
BODY_Y = WIN_Y + 16

# The pointer starts at the screen center (wserver WsState::new).
PTR_START = (512, 384)

DESKTOP = bytes([40, 40, 40])
TITLE_FOCUSED = bytes([64, 128, 192])
WHITE = bytes([255, 255, 255])

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


def send_paced(cmd, per_byte_ms=3):
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


def screendump(s):
    r = qmp_cmd(s, {"execute": "screendump", "arguments": {"filename": SCREEN}})
    if r is None or r.get("return") is None:
        return None
    time.sleep(0.5)
    with open(SCREEN, "rb") as f:
        data = f.read()
    parts = data.split(b"\n", 3)
    dims = parts[1].split()
    w, h = int(dims[0]), int(dims[1])
    return w, h, parts[3]


def mouse_move(s, tx, ty, steps=4):
    """Move the pointer to the absolute screen target (tx, ty). The
    virtio-tablet reports QEMU-normalized 0..0x7FFF positions; convert the
    screen target to that range (1024x768 bochs-display)."""
    vx = tx * 32768 // 1024
    vy = ty * 32768 // 768
    for _ in range(steps):
        r = qmp_cmd(s, {"execute": "input-send-event", "arguments": {
            "device": "fb0",
            "events": [
                {"type": "abs", "data": {"axis": "x", "value": vx}},
                {"type": "abs", "data": {"axis": "y", "value": vy}},
            ]
        }})
        if r is None or r.get("return") is None:
            print("MOUSE MOVE FAILED: %r" % r, file=sys.stderr)
            return False
        time.sleep(0.08)
    return True


def mouse_btn(s, down):
    r = qmp_cmd(s, {"execute": "input-send-event", "arguments": {
        "device": "fb0",
        "events": [{"type": "btn", "data": {"button": "left", "down": down}}]
    }})
    if r is None or r.get("return") is None:
        print("MOUSE BTN FAILED: %r" % r, file=sys.stderr)
        return False
    time.sleep(0.1)
    return True


def white_count(w, px, x0, y0, x1, y1):
    n = 0
    for y in range(y0, y1):
        off = (y * w + x0) * 3
        for x in range(x0, x1):
            if px[off:off + 3] == WHITE:
                n += 1
            off += 3
    return n


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    if not wait_for(lambda: b"wserver: ready" in out, 150):
        print("WSERVER NOT READY", file=sys.stderr)
        with lock:
            print(bytes(out[-600:]).decode(errors="replace"), file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    if not wait_for(lambda: b"# " in out, 30):
        print("NO PROMPT", file=sys.stderr)
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    q = socket.create_connection(("127.0.0.1", 4460), timeout=5)
    qmp_cmd(q, {"execute": "qmp_capabilities"})

    send_paced("wdemo info")
    time.sleep(1.5)

    def shot(tag):
        screen = screendump(q)
        if screen is None:
            print("SCREENDUMP FAILED (%s)" % tag, file=sys.stderr)
            q.close()
            qemu.kill()
            sys.exit(1)
        return screen

    w, h, px = shot("A")
    print("screen: %dx%d" % (w, h), flush=True)

    def pxat(x, y):
        off = (y * w + x) * 3
        return px[off:off + 3]

    # 1. Info window present + focused; the pointer arrow at the center.
    title_ok = pxat(WIN_X + 100, WIN_Y + 8) == TITLE_FOCUSED
    ptr0_ok = pxat(*PTR_START) == WHITE
    print("title=%s pointer@center=%s" % (title_ok, ptr0_ok), flush=True)

    # 2. Move the pointer onto the info title (200, 48).
    if not mouse_move(q, 200, 48):
        sys.exit(1)
    w, h, px = shot("B")
    ptr_moved = pxat(200, 48) == WHITE
    title_still = pxat(WIN_X + 60, 48) == TITLE_FOCUSED  # away from pointer
    print("pointer@title=%s title-still=%s" % (ptr_moved, title_still), flush=True)

    # 3. Drag the title bar +50, +30.
    mouse_btn(q, True)
    mouse_move(q, 250, 78, steps=2)
    mouse_btn(q, False)
    time.sleep(0.5)
    w, h, px = shot("C")
    # Old title pixel is desktop again; the new title shows focused blue.
    drag_moved = pxat(200, 48) == DESKTOP
    new_title = pxat(160, WIN_Y + 30 + 8) == TITLE_FOCUSED  # (90,70)+ → y 78
    print("old-title=desktop=%s new-title=%s" % (drag_moved, new_title), flush=True)

    # 4. Close button: the window is now at (90, 70); the X is at
    #    x 394..410, y 70..86.
    close_cx, close_cy = 402, 78
    if not mouse_move(q, close_cx, close_cy):
        sys.exit(1)
    mouse_btn(q, True)
    mouse_btn(q, False)
    time.sleep(0.5)
    w, h, px = shot("D")
    closed = (pxat(100, close_cy + 22) == DESKTOP
              and pxat(close_cx - 2, close_cy + 22) == DESKTOP)
    print("window-closed=%s" % closed, flush=True)

    # 5. wdemo ptr: click the body, then move the pointer away and check
    #    the '+' marker persists (the WS_PTR round trip).
    send_paced("wdemo ptr")
    time.sleep(1.5)
    w, h, px = shot("E")
    ptr_win_ok = pxat(WIN_X + 100, WIN_Y + 8) == TITLE_FOCUSED
    if not mouse_move(q, 200, 150):
        sys.exit(1)
    mouse_btn(q, True)
    mouse_btn(q, False)
    time.sleep(0.5)
    # Move the pointer far away so it no longer overlaps the marker cell.
    if not mouse_move(q, 900, 700, steps=5):
        sys.exit(1)
    w, h, px = shot("F")
    # Click at win-local (160, 94) → cell (row 5, col 20) at px (200, 136).
    marker = white_count(w, px, 192, 128, 216, 160) > 3
    print("ptr-window=%s marker=%s" % (ptr_win_ok, marker), flush=True)

    with lock:
        full = bytes(out).decode(errors="replace")
    print("=== FULL SERIAL (last 2000) ===")
    print(full[-2000:])
    import re
    for m in re.finditer(r"(wdemo:[^\r\n]*)", full):
        print("MATCH:", m.group(1))

    ok = title_ok and ptr0_ok and ptr_moved and title_still and drag_moved \
        and new_title and closed and ptr_win_ok and marker
    if not ok:
        with lock:
            print("--- serial tail ---", file=sys.stderr)
            print(bytes(out[-1500:]).decode(errors="replace"), file=sys.stderr)
    q.close()
    qemu.kill()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
