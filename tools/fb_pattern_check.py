#!/usr/bin/env python3
"""Check the framebuffer memory at PA 0xFD000000 for the test pattern
(red/green/blue thirds) and dump sample pixels."""
import socket
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display",
    "-m", "256M", "-no-reboot", "-s",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]

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


def gdb_packet(sock, payload):
    data = payload.encode()
    csum = sum(data) & 0xFF
    sock.sendall(b"$" + data + b"#%02x" % csum)
    resp = b""
    while True:
        b = sock.recv(1)
        if b == b"+":
            continue
        if b == b"$":
            resp = b""
            while True:
                c = sock.recv(1)
                if c == b"#":
                    sock.recv(2)
                    return resp
                resp += c
    return resp


def gdb_read_stop(sock, timeout):
    sock.settimeout(timeout)
    while True:
        try:
            b = sock.recv(1)
        except socket.timeout:
            return b"TIMEOUT"
        if b == b"+":
            continue
        if b == b"$":
            resp = b""
            while True:
                c = sock.recv(1)
                if c == b"#":
                    sock.recv(2)
                    return resp
                resp += c
    return b"TIMEOUT"


def read_mem(sock, addr, n):
    out_b = b""
    while n > 0:
        chunk = min(n, 4096)
        r = gdb_packet(sock, "m%x,%x" % (addr, chunk))
        out_b += bytes.fromhex(r.decode())
        addr += chunk
        n -= chunk
    return out_b


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    deadline = time.time() + 180
    while time.time() < deadline:
        with lock:
            data = bytes(out)
        if b"# " in data:
            break
        time.sleep(0.5)
    print("prompt seen: %s" % (b"# " in data), flush=True)

    sock = socket.create_connection(("127.0.0.1", 1234), timeout=5)
    try:
        sock.sendall(b"\x03")
        gdb_read_stop(sock, 5)
        time.sleep(0.5)

        FB_PA = 0xfd000000
        # 1024 px * 4 B = 4096 B per row; sample 3 pixels in row 0.
        row = read_mem(sock, FB_PA, 4096)
        for x in (0, 341, 511, 512, 682, 683, 1023):
            px = row[x * 4:x * 4 + 4]
            print("row0 x=%-4d -> %s" % (x, px.hex()), flush=True)
        # A few rows deeper.
        for y in (0, 383, 767):
            base = FB_PA + y * 4096
            row = read_mem(sock, base, 4096)
            px = row[512 * 4:512 * 4 + 4]
            print("row%d x=512 -> %s" % (y, px.hex()), flush=True)
        # Count nonzero bytes in the first 64 KiB.
        region = read_mem(sock, FB_PA, 65536)
        nz = sum(1 for b in region if b != 0)
        print("nonzero bytes in first 64 KiB of fb: %d / %d" % (nz, len(region)), flush=True)
    finally:
        sock.close()
    sys.exit(0)


if __name__ == "__main__":
    main()
