#!/usr/bin/env python3
"""Trace the fb server's init path with hardware breakpoints at the
fb/VM processes' LINEAR VAs, with proper step-over handling.

Freezes at the first enqueue (0x202c00), then arms Z1 (hw) breakpoints:
  - fb_server_main  (VA 0x10000e0)
  - BochsArch::init (VA 0x1000af0)
  - physmap_hook    (VA 0x1000080)
  - do_map_phys     (VA 0x1000430, VM server)
On each hit: remove the bp, single-step past it, re-arm, continue.
"""
import socket
import subprocess
import sys
import threading
import time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-monitor", "none",
    "-display", "none", "-vga", "none", "-device", "bochs-display",
    "-m", "256M", "-no-reboot", "-s", "-S",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
]

ENQUEUE_AND_START = 0x202c00
FB_MAIN_OFF = 0xe0
FB_INIT_OFF = 0xaf0
FB_PHYSMAP_OFF = 0x80
FB_ARCH_OFF = 0x4000
VM_DO_MAP_PHYS_OFF = 0x430
FB_VA_BASE = 0x1000000
VM_VA_BASE = 0x1000000

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


def regs_get(sock):
    r = gdb_packet(sock, "g")
    return bytes.fromhex(r.decode())


def read_u64(buf, off):
    return int.from_bytes(buf[off:off + 8], "little")


def read_mem(sock, addr, n):
    out_b = b""
    while n > 0:
        chunk = min(n, 4096)
        r = gdb_packet(sock, "m%x,%x" % (addr, chunk))
        out_b += bytes.fromhex(r.decode())
        addr += chunk
        n -= chunk
    return out_b


def parse_boot_phys(line, tag):
    idx = line.find(tag)
    if idx >= 0:
        for part in line[idx:idx + 70].split(b" "):
            if part.startswith(b"phys=0x"):
                return int(part[7:], 16)
    return None


def boot_log():
    with lock:
        return bytes(out)


def step_over(sock, addr):
    """Remove the hw-bp at addr, single-step past it, re-arm it."""
    gdb_packet(sock, "z1,%x,1" % addr)
    sock.sendall(b"$s#73")
    gdb_read_stop(sock, 10)
    gdb_packet(sock, "Z1,%x,1" % addr)


def main():
    global qemu
    qemu = subprocess.Popen(
        QEMU, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    threading.Thread(target=pump, daemon=True).start()

    sock = socket.create_connection(("127.0.0.1", 1234), timeout=5)
    try:
        gdb_packet(sock, "Z0,%x,1" % ENQUEUE_AND_START)
        sock.sendall(b"$c#63")
        first_stop = gdb_read_stop(sock, 90)
        print("first stop: %r" % first_stop[:30], flush=True)

        fb_phys = vm_phys = None
        for _ in range(100):
            data = boot_log()
            fb_phys = parse_boot_phys(data, b"/sbin/fb: loaded ")
            vm_phys = parse_boot_phys(data, b"/sbin/vm: loaded ")
            if fb_phys and vm_phys and b"enqueuing processes" in data:
                break
            time.sleep(0.1)
        print("fb_phys = 0x%x  vm_phys = 0x%x" % (fb_phys or 0, vm_phys or 0), flush=True)

        gdb_packet(sock, "z0,%x,1" % ENQUEUE_AND_START)
        bps = {
            "fb_server_main": FB_VA_BASE + FB_MAIN_OFF,
            "BochsArch::init": FB_VA_BASE + FB_INIT_OFF,
            "physmap_hook": FB_VA_BASE + FB_PHYSMAP_OFF,
            "do_map_phys": VM_VA_BASE + VM_DO_MAP_PHYS_OFF,
        }
        names = {}
        for name, addr in bps.items():
            r = gdb_packet(sock, "Z1,%x,1" % addr)
            print("Z1 %-16s 0x%x -> %r" % (name, addr, r), flush=True)
            names[addr] = name

        hits = {}
        for i in range(40):
            sock.sendall(b"$c#63")
            stop = gdb_read_stop(sock, 90)
            if stop == b"TIMEOUT":
                print("iteration %d: no more hits (timeout)" % i, flush=True)
                break
            regs = regs_get(sock)
            rip = read_u64(regs, 128)
            name = names.get(rip, "unknown")
            if name == "unknown":
                print("stop at unknown rip=%#x" % rip, flush=True)
                sock.sendall(b"$c#63")
                gdb_read_stop(sock, 10)
                continue
            hits[name] = hits.get(name, 0) + 1
            print("HIT %s #%d (rip=%#x)" % (name, hits[name], rip), flush=True)
            if rip == bps["physmap_hook"]:
                phys = read_u64(regs, 40)
                length = read_u64(regs, 32)
                print("  physmap_hook: phys=0x%x len=0x%x" % (phys, length), flush=True)
                if phys == 0xfd000000 or phys == 0xfebe8000:
                    arch = read_mem(sock, 0x3793000, 48)
                    print(
                        "  FB_ARCH now: base=0x%x size=0x%x regs=0x%x var.xres=%d"
                        % (
                            read_u64(arch, 0),
                            read_u64(arch, 8),
                            read_u64(arch, 16),
                            int.from_bytes(arch[24:28], "little", signed=True),
                        ),
                        flush=True,
                    )
            elif rip == bps["do_map_phys"]:
                # First arg (msg: &mut Message) is in RDI.
                msg = read_mem(sock, read_u64(regs, 40), 20)
                print(
                    "  do_map_phys: src=%d m_type=0x%x target=%d len=%d phys=0x%x"
                    % (
                        int.from_bytes(msg[0:4], "little", signed=True),
                        int.from_bytes(msg[4:8], "little", signed=True),
                        int.from_bytes(msg[8:12], "little", signed=True),
                        int.from_bytes(msg[12:16], "little", signed=True),
                        int.from_bytes(msg[16:20], "little"),
                    ),
                    flush=True,
                )
            step_over(sock, rip)
        else:
            sock.sendall(b"$c#63")
            gdb_read_stop(sock, 10)

        # Interrupt the running target, then read FB_ARCH.
        sock.sendall(b"\x03")
        gdb_read_stop(sock, 5)
        time.sleep(1)
        arch = read_mem(sock, fb_phys + FB_ARCH_OFF, 24)
        print(
            "FB_ARCH: base=0x%x size=0x%x regs=0x%x"
            % (read_u64(arch, 0), read_u64(arch, 8), read_u64(arch, 16)),
            flush=True,
        )
        print("hits: %s" % hits, flush=True)
        print("--- boot log tail ---", flush=True)
        print(boot_log()[-600:].decode(errors="replace"), flush=True)
    finally:
        sock.close()
    sys.exit(0)


if __name__ == "__main__":
    main()
