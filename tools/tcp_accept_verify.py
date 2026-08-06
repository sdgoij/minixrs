"""Boot AArch64, run the guest TCP echo server (tcpserver), connect from the
host via hostfwd, and assert a full echo round-trip through listen/accept.
Usage: tcp_accept_verify.py <aarch64|riscv64|x86>"""
import socket
import subprocess
import sys
import threading
import time

ARCH = sys.argv[1]
HOST_PORT = 28000
GUEST_PORT = 20000

RESULT = {}


def host_client():
    """Connect to the guest's tcpserver via hostfwd, send data, read echo."""
    try:
        s = socket.create_connection(("127.0.0.1", HOST_PORT), timeout=30)
        s.settimeout(30)
        s.sendall(b"hello-from-host")
        data = b""
        while len(data) < len(b"hello-from-host"):
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        s.close()
        RESULT["echo"] = data
    except Exception as e:  # noqa: BLE001 - test tooling
        RESULT["error"] = repr(e)


if ARCH == "aarch64":
    QEMU = [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
        "-nographic", "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT}",
        "-device", "virtio-net-device,netdev=net0",
        "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
    ]
elif ARCH == "riscv64":
    QEMU = [
        "qemu-system-riscv64", "-machine", "virt", "-m", "256M", "-nographic", "-no-reboot",
        "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", f"user,id=net0,hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT}",
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
        "-netdev", f"user,id=net0,hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT}",
        "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
    ]

log = open(f"target/tcp_accept_{ARCH}.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


time.sleep(20)
send(f"tcpserver {GUEST_PORT}\n")
time.sleep(8)

threading.Thread(target=host_client, daemon=True).start()
time.sleep(20)
p.kill()
p.wait(timeout=5)
log.close()

d = open(f"target/tcp_accept_{ARCH}.log", "rb").read().decode("utf-8", "replace")
print("=== serial tail ===")
print(d[-600:])
echo = RESULT.get("echo")
if RESULT.get("error"):
    print("HOST CLIENT ERROR:", RESULT["error"])
    sys.exit(1)
if echo == b"hello-from-host" and "accepted" in d:
    print("ACCEPT ECHO OK: guest echoed", echo)
    sys.exit(0)
print("ACCEPT ECHO FAILED: got", echo)
sys.exit(1)
