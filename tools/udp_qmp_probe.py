#!/usr/bin/env python3
"""Boot AArch64 with an HMP monitor, warm ARP, hang udp, dump process state via xp."""
import socket
import subprocess
import time

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-device,netdev=net0",
    "-monitor", "tcp:127.0.0.1:5555,server,nowait",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

PROC_TABLE = 0x40030C80
STRIDE = 0x370
N_TASKS = 5
CUR_PROC = 0x40068E18  # BOOT_CPU_STORAGE

log = open("target/udp_qmp_a64.log", "wb")
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


def xp(s, fmt, addr):
    s.sendall(f"xp /{fmt} 0x{addr:x}\n".encode())
    out = b""
    try:
        while not out.endswith(b"(qemu) "):
            out += s.recv(4096)
    except socket.timeout:
        pass
    text = out.decode(errors="replace")
    # HMP echoes the command first, then prints "ADDR: VALUE".
    for line in text.splitlines():
        if ":" in line and not line.strip().startswith("xp "):
            return line.split(":", 1)[1].strip()
    return "0"


def val(v):
    v = v.strip().split()[0] if v.strip() else "0"
    try:
        return int(v, 16)
    except ValueError:
        return 0


time.sleep(20)
send("ping 10.0.2.3\n")
time.sleep(6)
send("udp example.com\n")
time.sleep(10)  # let it hang

m = qmp()
cur = val(xp(m, "gx", CUR_PROC))
print(f"current_proc=0x{cur:x} slot={(cur - PROC_TABLE) // STRIDE - N_TASKS}")
for slot in range(0, 21):
    base = PROC_TABLE + (N_TASKS + slot) * STRIDE
    nr = val(xp(m, "xw", base + 320))
    rts = val(xp(m, "xw", base + 336))
    misc = val(xp(m, "xw", base + 340))
    gf = val(xp(m, "xw", base + 496))
    st = val(xp(m, "xw", base + 500))
    ep = val(xp(m, "xw", base + 544))
    name = xp(m, "16cb", base + 528).strip()
    elr = val(xp(m, "gx", base + 256))
    print(f"slot {slot:2d} nr={nr:2d} rts=0x{rts:08x} misc=0x{misc:08x} "
          f"getfrom={gf} sendto={st} ep={ep} elr=0x{elr:x} name={name}")
m.close()
p.kill()
p.wait(timeout=5)
log.close()
d = open("target/udp_qmp_a64.log", "rb").read().decode("utf-8", "replace")
print("=== serial tail ===")
print(d[-300:])
