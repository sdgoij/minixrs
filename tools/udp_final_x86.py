"""Boot x86_64: udp DNS test then ping regression, dump the tail."""
import subprocess, time

QEMU = [
    "qemu-system-x86_64", "-nographic", "-m", "256M", "-no-reboot",
    "-kernel", "target/trampoline.elf",
    "-device", "loader,file=target/kernel.bin,addr=0x200000",
    "-drive", "if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough",
    "-device", "virtio-blk-pci,disable-legacy=on,drive=disk0",
    "-netdev", "user,id=net0",
    "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
]

log = open("target/udp_final_x86.log", "wb")
p = subprocess.Popen(QEMU, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)


def send(s, delay=0.05):
    for b in s.encode():
        p.stdin.write(bytes([b]))
        p.stdin.flush()
        time.sleep(delay)


time.sleep(20)
send("udp example.com\n")
time.sleep(14)
send("ping 10.0.2.2\n")
time.sleep(6)
p.kill()
p.wait(timeout=5)
log.close()

d = open("target/udp_final_x86.log", "rb").read().decode("utf-8", "replace")
print(d[-600:])
