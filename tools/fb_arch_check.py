#!/usr/bin/env python3
"""Read FB_ARCH at the REAL field offsets (dev.base @ +0x60, dev.size
@ +0x68, regs @ +0xc0) post-boot, and dump the whole struct."""
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


def u64(b, o):
    return int.from_bytes(b[o:o + 8], "little")


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
        FB_PHYS = 0x378f000
        arch = read_mem(sock, FB_PHYS + 0x4000, 0xd0)
        # BochsArch is repr(C): dev (base,size) @0, regs @16, var @24,
        # fix @120.
        print("FB_ARCH (repr(C) offsets):")
        print("  dev.base  @+0x00 = %#x" % u64(arch, 0x00))
        print("  dev.size  @+0x08 = %#x" % u64(arch, 0x08))
        print("  regs      @+0x10 = %#x" % u64(arch, 0x10))
        # dump the whole struct
        for i in range(0, 0xd0, 16):
            print("  +0x%03x: %s" % (i, arch[i:i + 16].hex()))
    finally:
        sock.close()
    sys.exit(0)


if __name__ == "__main__":
    main()
