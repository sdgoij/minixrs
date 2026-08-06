"""Boot AArch64 with hostfwd on two ports: echo test (host responds) and a
closed-port test (host refuses -> RST -> ECONNREFUSED)."""
import socket
import subprocess
import threading
import time

ECHO = 18080
REFUSED = 18081  # forwarded but no host listener -> RST


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

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", f"user,id=net0,hostfwd=tcp::{ECHO}-:{ECHO},hostfwd=tcp::{REFUSED}-:{REFUSED}",
    "-device", "virtio-net-device,netdev=net0",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

log = open("target/tcp_full_a64.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


time.sleep(20)
send(f"tcp 10.0.2.2 {ECHO} hello-from-minix\n")
time.sleep(10)
send(f"tcp 10.0.2.2 {REFUSED} x\n")
time.sleep(10)
send("ping 10.0.2.2\n")
time.sleep(5)
p.kill()
p.wait(timeout=5)
log.close()

d = open("target/tcp_full_a64.log", "rb").read().decode("utf-8", "replace")
print(d[-700:])
