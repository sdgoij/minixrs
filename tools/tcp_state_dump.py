#!/usr/bin/env python3
"""Boot AArch64, run tcpserver, drive 3 host connections, then dump the net
server's TCP socket table + listener accept queue via QMP."""
import socket
import subprocess
import threading
import time

HOST_PORT = 28000
GUEST_PORT = 20000

# net phys base 0x42115000 (from the boot log) + (TCP_SOCKETS 0x100f000 -
# 0x1000000) -> 0x42124000. TcpSock stride 0x3108 (probe_offsets).
SOCK0 = 0x42124000
STRIDE = 0x3108
# Field offsets (probe_offsets): state@30, snd_una@40, rcv_nxt@44, err@48,
# rx_len@56, tx_len@2112, cookie_set@4188, accept_queue@4200.
OFF_STATE = 30
OFF_RX_LEN = 56
OFF_TX_LEN = 2112
OFF_QUEUE = 4200
# PendingConn: in_use@0, established@1, rem_port@6, rx_len@32, size 0x828.
P_IN_USE = 0
P_EST = 1
P_PORT = 6
P_RX_LEN = 32
P_SIZE = 0x828

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", f"user,id=net0,hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT}",
    "-device", "virtio-net-device,netdev=net0",
    "-object", "filter-dump,id=fdump,netdev=net0,file=target/netdump.pcap",
    "-monitor", "tcp:127.0.0.1:5555,server,nowait",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

log = open("target/tcp_state.log", "wb")
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


def host_roundtrip(tag, port, payload):
    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=30)
        s.settimeout(30)
        print(f"HOST {tag}: local port {s.getsockname()[1]}")
        s.sendall(payload)
        data = b""
        while len(data) < len(payload):
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        s.close()
        print(f"HOST {tag}: got {data!r}")
    except Exception as e:  # noqa: BLE001
        print(f"HOST {tag}: error {e!r}")


time.sleep(20)
send(f"tcpserver {GUEST_PORT}\n")
time.sleep(8)
for i in range(3):
    threading.Thread(
        target=host_roundtrip, args=(f"conn{i}", HOST_PORT, f"msg-{i}-from-host".encode()),
        daemon=True,
    ).start()
    time.sleep(6)
time.sleep(3)

m = qmp()
for i in range(4):
    base = SOCK0 + i * STRIDE
    in_use = xp(m, "xb", base)
    minor = xp(m, "xw", base + 4)
    state = xp(m, "xb", base + OFF_STATE)
    rem_port = xp(m, "xw", base + 14)
    rx = xp(m, "gx", base + OFF_RX_LEN)
    tx = xp(m, "gx", base + OFF_TX_LEN)
    print(
        f"slot{i}: in_use={in_use} minor={minor} state={state} "
        f"rem_port={rem_port} rx_len={rx} tx_len={tx}"
    )
    for q in range(4):
        qb = base + OFF_QUEUE + q * P_SIZE
        in_use = xp(m, "xb", qb + P_IN_USE)
        est = xp(m, "xb", qb + P_EST)
        port = xp(m, "xw", qb + P_PORT)
        prx = xp(m, "gx", qb + P_RX_LEN)
        if in_use != "0x00":
            print(f"  queue[{q}]: in_use={in_use} est={est} rem_port={port} rx_len={prx}")
m.close()
p.kill()
p.wait(timeout=5)
log.close()
d = open("target/tcp_state.log", "rb").read().decode("utf-8", "replace")
print("=== serial tail ===")
print(d[-400:])
