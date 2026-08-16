#!/usr/bin/env python3
"""L3 probe: the virtio-keyboard input driver on riscv64/aarch64.

Boots with `-device virtio-gpu-device -device virtio-keyboard-device`,
waits for the input server to report `input: virtio-keyboard ready`
(the driver probed ID 18 and armed its alarm poll), then runs the
wserver desktop flow (`wdemo info`, `wdemo term 2`) and injects `a` +
`b` via QMP sendkey. The key acceptance is `wdemo: term ok`: both keys
routed virtio-keyboard -> input server (SYS_SETALARM poll wake) ->
wserver -> the focused term window. A screendump then asserts the
glyphs at the same pixels the x86 wserver probe checks.

Usage: python tools/virtio_l3_probe.py [riscv64|aarch64]
"""
import json
import os
import select
import socket
import subprocess
import sys
import threading
import time

ARCH = sys.argv[1] if len(sys.argv) > 1 else "riscv64"

if ARCH == "riscv64":
    QEMU = [
        "qemu-system-riscv64", "-machine", "virt", "-m", "256M",
        "-nographic", "-monitor", "none", "-display", "none",
        "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
        "-qmp", "tcp:127.0.0.1:4457,server=on,wait=off",
        "-drive", "if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", "user,id=net0",
        "-device", "virtio-net-device,netdev=net0",
        "-device", "virtio-gpu-device",
        "-device", "virtio-keyboard-device",
        "-kernel", "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64",
    ]
    PORT = 4457
elif ARCH == "aarch64":
    QEMU = [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57",
        "-m", "256M", "-nographic", "-monitor", "none", "-display", "none",
        "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
        "-qmp", "tcp:127.0.0.1:4458,server=on,wait=off",
        "-drive", "if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", "user,id=net0",
        "-device", "virtio-net-device,netdev=net0",
        "-device", "virtio-gpu-device",
        "-device", "virtio-keyboard-device",
        "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
    ]
    PORT = 4458
else:
    print(f"unknown arch {ARCH!r} (want riscv64|aarch64)")
    sys.exit(2)

SCREEN = f"tools/l3_{ARCH}.ppm"

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
    except Exception:
        pass


def qmp_cmd(s, obj, timeout=60):
    s.settimeout(timeout)
    s.sendall((json.dumps(obj) + "\n").encode())
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r, _, _ = select.select([s], [], [], max(0.1, deadline - time.time()))
        if not r:
            continue
        chunk = s.recv(65536)
        if not chunk:
            return None
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if "return" in o or "error" in o:
                return o
    return None


def wait_for(pred, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with lock:
            if pred():
                return True
        time.sleep(0.1)
    with lock:
        return pred()


def send_paced(cmd, per_byte_ms=30):
    # Slow pacing: the riscv UART drops piped bursts under -qmp (16550
    # FIFO overrun while SIE=0); 30 ms/byte keeps the RX stream below
    # what the timer-tick drain sustains.
    for ch in cmd.encode() + b"\n":
        qemu.stdin.write(bytes([ch]))
        qemu.stdin.flush()
        time.sleep(per_byte_ms / 1000.0)


def tail():
    with lock:
        return bytes(out[-4000:]).decode(errors="replace")


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.PIPE
    )
    threading.Thread(target=pump, daemon=True).start()

    ok = True

    ready = wait_for(lambda: b"input: virtio-keyboard ready" in out, 300)
    print(f"[1] virtio-keyboard ready: {ready}")
    ok &= ready

    booted = wait_for(lambda: b"starting shell" in out, 300)
    print(f"[2] boot to shell: {booted}")
    ok &= booted
    if not (ready and booted):
        print("--- serial tail ---")
        print(tail())
        qemu.kill()
        sys.exit(1)

    # The shell must be idle at its prompt before typing: the riscv UART
    # only drains RX in user mode (KNOWN_ISSUES [riscv] #1), so bytes
    # typed before the prompt is up overrun the 16550 FIFO.
    prompt = wait_for(lambda: b"# " in out, 120)
    print(f"[3] shell prompt: {prompt}")
    ok &= prompt
    if not prompt:
        print("--- serial tail ---")
        print(tail())
        qemu.kill()
        sys.exit(1)
    time.sleep(1)

    send_paced("wdemo info")
    info_ok = wait_for(lambda: b"wdemo: info ok" in out, 60)
    print(f"[4] wdemo info: {info_ok}")
    ok &= info_ok

    send_paced("wdemo term 2")
    term_wait = wait_for(lambda: b"wdemo: term waiting" in out, 60)
    print(f"[5] wdemo term waiting: {term_wait}")
    ok &= term_wait
    if not (info_ok and term_wait):
        print("--- serial tail ---")
        print(tail())
        qemu.kill()
        sys.exit(1)
    time.sleep(0.5)

    s = socket.create_connection(("127.0.0.1", PORT), timeout=10)
    qmp_cmd(s, {"execute": "qmp_capabilities"})

    def sendkey(k):
        qmp_cmd(s, {"execute": "human-monitor-command",
                    "arguments": {"command-line": f"sendkey {k}"}})

    sendkey("a")
    sendkey("b")

    # Key acceptance: both keys reached the focused term window through
    # the virtio-keyboard path.
    term_ok = wait_for(lambda: b"wdemo: term ok" in out, 60)
    print(f"[6] wdemo term ok (2 keys routed): {term_ok}")
    ok &= term_ok

    try:
        os.unlink(SCREEN)
    except OSError:
        pass
    r = qmp_cmd(s, {"execute": "human-monitor-command",
                    "arguments": {"command-line": f"screendump {SCREEN}"}})
    resp_ok = bool(r) and r.get("error") is None
    print(f"[7] screendump resp_ok={resp_ok}")
    ok &= resp_ok

    s.close()
    qemu.kill()
    qemu.wait()

    # Parse the PPM and assert the desktop + glyph pixels (PPM P6 = R,G,B).
    dumped = False
    for _ in range(600):
        try:
            with open(SCREEN, "rb") as f:
                if f.read(3).startswith(b"P6"):
                    dumped = True
                    break
        except OSError:
            pass
        time.sleep(0.1)
    print(f"[8] capture file P6: {dumped}")
    ok &= dumped
    if not dumped:
        print(f"=== L3 {ARCH}: FAIL (no capture) ===")
        sys.exit(1)

    with open(SCREEN, "rb") as f:
        f.readline()  # P6
        dims = f.readline().split()
        f.readline()  # maxval
        w, h = int(dims[0]), int(dims[1])
        px = f.read()
    print(f"[9] capture {w}x{h}")
    ok &= w == 1024 and h == 768

    def pxat(x, y):
        off = (y * w + x) * 3
        return px[off:off + 3]

    DESKTOP = bytes([40, 40, 40])
    TITLE_FOCUSED = bytes([64, 128, 192])
    TITLE_UNFOCUSED = bytes([32, 32, 64])
    WHITE = bytes([255, 255, 255])
    RED = bytes([255, 0, 0])
    GREEN = bytes([0, 255, 0])
    BLUE = bytes([0, 0, 255])

    bg = pxat(10, 400) == DESKTOP
    info_title = pxat(100, 48) == TITLE_UNFOCUSED
    strip = (pxat(60, 210) == RED and pxat(60, 218) == GREEN
             and pxat(60, 226) == BLUE)
    term_title = pxat(500, 48) == TITLE_FOCUSED
    a_glyph = pxat(457, 61) == WHITE
    b_glyph = pxat(466, 61) == WHITE
    print(f"[10] desktop={bg} info-title={info_title} strip={strip} "
          f"term-title={term_title} a-glyph={a_glyph} b-glyph={b_glyph}")
    ok &= bg and info_title and strip and term_title and a_glyph and b_glyph

    print(f"=== L3 {ARCH}: {'PASS' if ok else 'FAIL'} ===")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
