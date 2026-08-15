#!/usr/bin/env python3
import subprocess, sys, threading, time
qemu = subprocess.Popen(
    ["qemu-system-x86_64","-nographic","-monitor","none","-m","256M","-no-reboot",
     "-kernel","target/trampoline.elf","-device","loader,file=target/kernel.bin,addr=0x200000",
     "-drive","if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough",
     "-device","virtio-blk-pci,disable-legacy=on,drive=disk0"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
lock = threading.Lock(); out = bytearray()
def pump():
    while True:
        c = qemu.stdout.read1(65536)
        if not c: break
        with lock: out.extend(c)
threading.Thread(target=pump, daemon=True).start()
def wait_for(pred, timeout, base=0):
    dl = time.time()+timeout
    while time.time()<dl:
        with lock:
            if pred(base): return True
        time.sleep(0.05)
    with lock: return pred(base)
def has_prompt(b): return b"# " in out[b:]
def send(cmd):
    qemu.stdin.write(cmd.encode()+b"\n"); qemu.stdin.flush()
if not wait_for(has_prompt, 90):
    print("NO PROMPT"); sys.exit(1)
time.sleep(0.5)
base = len(out)
send("su test")
if not wait_for(lambda b: b"Password:" in out[b:], 15, base):
    print("NO PASSWORD PROMPT"); sys.exit(1)
send("test123")
time.sleep(3)
send("id")
time.sleep(2)
send("exit")
time.sleep(3)
with lock: chunk = bytes(out[base:])
print(chunk.decode(errors="replace")[:1500])
