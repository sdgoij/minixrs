"""M6 persistence test: write PERSIST to /p.txt, sync, graceful QEMU quit,
check the host disk image, then boot again and read /p.txt back."""
import subprocess, time, socket, re

def boot(with_monitor, tag):
    q = ["qemu-system-x86_64", "-nographic", "-m", "256M", "-no-reboot",
         "-kernel", "target/trampoline.elf",
         "-device", "loader,file=target/kernel.bin,addr=0x200000",
         "-drive", "if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
         "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0"]
    if with_monitor:
        q += ["-monitor", "tcp:127.0.0.1:5556,server,nowait"]
    log = open(f"target/persist_{tag}.log", "wb")
    p = subprocess.Popen(q, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
    time.sleep(14)
    return p, log

def send(p, s):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(0.03)

# Session 1: write + sync, then hard-kill (writethrough keeps the writes).
p, log = boot(True, 1)
send(p, "echo PERSIST > /p.txt\n")
time.sleep(3)
send(p, "sync\n")
time.sleep(3)
p.kill()
p.wait(timeout=5)
log.close()

d = open("target/images/x86_64-pc-minix/disk.img", "rb").read()
print("PERSIST in disk image:", d.count(b"PERSIST"))

# Session 2: read back.
# Session 2: read back.
p2, log2 = boot(False, 2)
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
d2 = open("target/persist_1.log", "rb").read().decode("utf-8", "replace")
print("--- session 1 tail ---")
print(repr(d2[d2.find("echo PERSIST"):]))
d3 = open("target/persist_2.log", "rb").read().decode("utf-8", "replace")
print("--- session 2 tail ---")
print(repr(d3[d3.find("cat /p.txt"):]))
