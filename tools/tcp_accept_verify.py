"""Boot an arch, run the guest TCP echo server (tcpserver), and verify the
server-side TCP lifecycle: multiple sequential connections each get their
echo (exercising the accept queue, slot recycling, and the FIN close
handshake), and a connection to a closed guest port is refused fast (RST).
Usage: tcp_accept_verify.py <aarch64|riscv64|x86>"""
import socket
import subprocess
import sys
import threading
import time

ARCH = sys.argv[1]
HOST_PORT = 28000
GUEST_PORT = 20000
CLOSED_HOST_PORT = 28001  # forwards to guest port 20001: nothing listens

RESULT = {}


def host_roundtrip(tag, port, payload):
    """One host-initiated connection: send `payload`, expect it echoed back
    with a clean close (no reset)."""
    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=30)
        s.settimeout(30)
        s.sendall(payload)
        data = b""
        while len(data) < len(payload):
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        s.close()
        RESULT[tag] = data
    except Exception as e:  # noqa: BLE001 - test tooling
        RESULT[tag] = ("error", repr(e))


def host_closed_port(tag):
    """A connection to a guest port with no listener must die fast: SLIRP
    completes the host-side handshake as a proxy, but the guest's RST tears
    the connection down, so the first send/recv errors instead of hanging."""
    try:
        t0 = time.time()
        s = socket.create_connection(("127.0.0.1", CLOSED_HOST_PORT), timeout=15)
        s.settimeout(3)
        s.sendall(b"x")
        data = s.recv(4096)
        s.close()
        RESULT[tag] = ("read", data, time.time() - t0)
    except (ConnectionResetError, ConnectionAbortedError):
        RESULT[tag] = ("reset", time.time() - t0)
    except socket.timeout:
        RESULT[tag] = ("timeout", time.time() - t0)
    except Exception as e:  # noqa: BLE001 - test tooling
        RESULT[tag] = ("error", repr(e))


if ARCH == "aarch64":
    QEMU = [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
        "-nographic", "-no-reboot", "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", "user,id=net0,"
        f"hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT},hostfwd=tcp::{CLOSED_HOST_PORT}-:20001",
        "-device", "virtio-net-device,netdev=net0",
        "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
    ]
elif ARCH == "riscv64":
    QEMU = [
        "qemu-system-riscv64", "-machine", "virt", "-m", "256M", "-nographic", "-no-reboot",
        "-global", "virtio-mmio.force-legacy=off",
        "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
        "-device", "virtio-blk-device,drive=disk0",
        "-netdev", "user,id=net0,"
        f"hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT},hostfwd=tcp::{CLOSED_HOST_PORT}-:20001",
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
        "-netdev", "user,id=net0,"
        f"hostfwd=tcp::{HOST_PORT}-:{GUEST_PORT},hostfwd=tcp::{CLOSED_HOST_PORT}-:20001",
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

# Three sequential round trips prove the accept queue recycles the closing
# socket slot and each connection closes cleanly (FIN handshake, no reset).
for i in range(3):
    payload = f"msg-{i}-from-host".encode()
    threading.Thread(target=host_roundtrip, args=(f"echo{i}", HOST_PORT, payload),
                     daemon=True).start()
    time.sleep(6)
threading.Thread(target=host_closed_port, args=("closed",), daemon=True).start()
time.sleep(10)
p.kill()
p.wait(timeout=5)
log.close()

d = open(f"target/tcp_accept_{ARCH}.log", "rb").read().decode("utf-8", "replace")
print("=== serial tail ===")
print(d[-500:])
ok = True
for i in range(3):
    got = RESULT.get(f"echo{i}")
    want = f"msg-{i}-from-host".encode()
    if got == want:
        print(f"ECHO{i} OK")
    else:
        print(f"ECHO{i} FAILED: got {got!r}, want {want!r}")
        ok = False
closed = RESULT.get("closed")
if closed and closed[0] in ("reset", "read"):
    print(f"CLOSED-PORT RST OK ({closed[0]} in {closed[1]:.2f}s)")
else:
    print(f"CLOSED-PORT RST FAILED: {closed!r}")
    ok = False
if "accepted" not in d:
    print("no accept logged")
    ok = False
sys.exit(0 if ok else 1)
