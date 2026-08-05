"""M6 persistence test for AArch64 (virtio-mmio): write PERSIST to /p.txt,
sync, kill QEMU (writethrough), check the host disk image, then boot again
and read /p.txt back."""
import subprocess, time

QEMU = [
    "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a57", "-m", "256M",
    "-nographic", "-no-reboot",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-kernel", "target/aarch64-unknown-minix/release/kernel-boot-aarch64",
]

def boot(tag):
    log = open(f"target/persist_a64_{tag}.log", "wb")
    p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
    time.sleep(20)
    return p, log

def send(p, s):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(0.03)

# Session 1: write + sync, then hard-kill (writethrough keeps the writes).
p, log = boot(1)
send(p, "echo PERSIST > /p.txt\n")
time.sleep(3)
send(p, "sync\n")
time.sleep(3)
p.kill()
p.wait(timeout=5)
log.close()

d = open("target/disk.img", "rb").read()
print("PERSIST in disk image:", d.count(b"PERSIST"))

# Session 2: read back.
p2, log2 = boot(2)
send(p2, "cat /p.txt\n")
time.sleep(3)
send(p2, "ls /\n")
time.sleep(3)
p2.stdin.close()
try:
    p2.wait(timeout=5)
except subprocess.TimeoutExpired:
    p2.kill()
log2.close()
d1 = open("target/persist_a64_1.log", "rb").read().decode("utf-8", "replace")
print("--- session 1 tail ---")
print(repr(d1[d1.find("echo PERSIST"):]))
d2 = open("target/persist_a64_2.log", "rb").read().decode("utf-8", "replace")
print("--- session 2 tail ---")
print(repr(d2[d2.find("cat /p.txt"):]))
