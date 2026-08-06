"""Boot an arch, run the TCP echo test against a host server, print the tail.
Usage: tcp_verify_arch.py <aarch64|riscv64|x86>"""
import socket
import subprocess
import sys
import threading
import time

ARCH = sys.argv[1]
ECHO = 18080


def echo_server():
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", ECHO))
    s.listen(1)
    conn, _ = s.accept()
    conn.settimeout(25)
    data = conn.recv(4096)
    conn.sendall(b"host says: " + data)
    conn.close()
    s.close()


threading.Thread(target=echo_server, daemon=True).start()

if ARCH == "aarch64":
    QEMU = [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
        "-nographic", "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{ECHO}-:{ECHO}",
        "-device", "virtio-net-device,netdev=net0",
        "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
    ]
elif ARCH == "riscv64":
    QEMU = [
        "qemu-system-riscv64", "-machine", "virt", "-m", "256M", "-nographic", "-no-reboot",
        "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{ECHO}-:{ECHO}",
        "-device", "virtio-net-device,netdev=net0",
        "-kernel", "target/riscv64gc-unknown-none-elf/release/kernel-boot-riscv64",
    ]
else:  # x86
    QEMU = [
        "qemu-system-x86_64", "-nographic", "-m", "256M", "-no-reboot",
        "-kernel", "target/trampoline.elf",
        "-device", "loader,file=target/kernel.bin,addr=0x200000",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{ECHO}-:{ECHO}",
        "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
    ]

log = open(f"target/tcp_{ARCH}.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


time.sleep(20)
send(f"tcp 10.0.2.2 {ECHO} hello-from-minix\n")
time.sleep(12)
send("ping 10.0.2.2\n")
time.sleep(5)
p.kill()
p.wait(timeout=5)
log.close()

d = open(f"target/tcp_{ARCH}.log", "rb").read().decode("utf-8", "replace")
print(d[-500:])
