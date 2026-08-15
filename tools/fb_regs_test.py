#!/usr/bin/env python3
"""Test the bochs-display register BAR directly via gdb: perform the
mode-set dance at PA 0xFEBE8000 and read back XRES."""
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

REGS = 0xfebe8000
FB = 0xfd000000

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


def write_mem(sock, addr, data):
    r = gdb_packet(sock, "M%x,%x:%s" % (addr, len(data), data.hex()))
    return r


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

        print("register BAR raw read (16 bytes): %s"
              % read_mem(sock, REGS, 16).hex(), flush=True)

        def wr16(off, val):
            r = write_mem(sock, REGS + off, val.to_bytes(2, "little"))
            print("  write reg[%d]=0x%x -> %r" % (off // 2, val, r), flush=True)

        def rd16(off):
            b = read_mem(sock, REGS + off, 2)
            v = int.from_bytes(b, "little")
            print("  read  reg[%d] = 0x%x" % (off // 2, v), flush=True)
            return v

        print("mode-set dance via gdb:", flush=True)
        wr16(0, 0)          # ENABLE = 0
        wr16(2, 1024)       # XRES
        wr16(4, 768)        # YRES
        wr16(6, 32)         # BPP
        wr16(8, 0x41)       # ENABLE = enabled | LFB
        xres = rd16(2)
        print("XRES readback = %d (expect 1024)" % xres, flush=True)

        # If the mode-set worked, the fb surface should be 1024x768 and
        # writable — poke a pixel.
        r = write_mem(sock, FB, b"\x00\x00\xff\x00")
        print("pixel write -> %r" % r, flush=True)
        print("pixel readback: %s" % read_mem(sock, FB, 4).hex(), flush=True)
        r = write_mem(sock, FB + 4096 * 100 + 4 * 512, b"\xff\x00\x00\x00")
        print("pixel2 write -> %r" % r, flush=True)
        print("pixel2 readback: %s" % read_mem(sock, FB + 4096 * 100 + 4 * 512, 4).hex(), flush=True)
    finally:
        sock.close()
    sys.exit(0)


if __name__ == "__main__":
    main()
