#!/usr/bin/env python3
"""Boot AArch64, warm ARP, run udp until send fails, then dump net server state."""
import socket
import subprocess
import time

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-device,netdev=net0",
    "-monitor", "tcp:127.0.0.1:5555,server,nowait",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

STATE = 0x4211B000  # 0x42115000 + (0x1006000 - 0x1000000)
SOCKETS = 0x4211E800  # 0x42115000 + (0x1009800 - 0x1000000)

log = open("target/udp_netstate.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


def qmp():
    s = socket.create_connection(("127.0.0.1", 5555), timeout=5)
    s.settimeout(1.0)
    time.sleep(0.3)
    buf = b""
    try:
        while b"(qemu) " not in buf:
            buf += s.recv(4096)
    except socket.timeout:
        pass
    return s


def xp(s, fmt, addr, count=1):
    s.sendall(f"xp /{count}{fmt} 0x{addr:x}\n".encode())
    out = b""
    try:
        while not out.endswith(b"(qemu) "):
            out += s.recv(4096)
    except socket.timeout:
        pass
    text = out.decode(errors="replace")
    for line in text.splitlines():
        if ":" in line and not line.strip().startswith("xp "):
            return line.split(":", 1)[1].strip()
    return "?"


time.sleep(20)
send("ping 10.0.2.3\n")
time.sleep(6)
send("udp example.com\n")
time.sleep(4)  # send fails fast now

m = qmp()
print("STATE.mac     :", xp(m, "cb", STATE, 6))
print("STATE.arp_len :", xp(m, "xb", STATE + 88))
print("STATE.arp[0]  :", xp(m, "10cb", STATE + 8, 10))
print("STATE.arp[1]  :", xp(m, "10cb", STATE + 18, 10))
for i in range(3):
    base = SOCKETS + i * 0x820
    print(f"SOCKETS[{i}] in_use={xp(m, 'xb', base)} minor={xp(m, 'xw', base + 4)} "
          f"flags=0x{xp(m, 'xw', base + 8)} loc_port={xp(m, 'xw', base + 12)} "
          f"rem_port={xp(m, 'xw', base + 14)} loc_addr={xp(m, '4cb', base + 16, 4)} "
          f"rem_addr={xp(m, '4cb', base + 20, 4)} rx_len={xp(m, 'gx', base + 24)}")
m.close()
p.kill()
p.wait(timeout=5)
log.close()
d = open("target/udp_netstate.log", "rb").read().decode("utf-8", "replace")
print("=== serial tail ===")
print(d[-200:])
