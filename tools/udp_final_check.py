"""Boot AArch64: udp DNS test then ping regression, dump the tail."""
import subprocess, time

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-device,netdev=net0",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

log = open("target/udp_final_a64.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


time.sleep(20)
send("udp example.com\n")
time.sleep(12)
send("udp example.org\n")
time.sleep(12)
send("ping 10.0.2.2\n")
time.sleep(6)
p.kill()
p.wait(timeout=5)
log.close()

d = open("target/udp_final_a64.log", "rb").read().decode("utf-8", "replace")
print(d[-700:])
