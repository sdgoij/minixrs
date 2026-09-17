#!/usr/bin/env python3
"""Dump the RISC-V virtio-blk transport state out of a running QEMU.

Boots the riscv64 image with a QMP socket and then reads, from the host:

  * the virtio-mmio registers of the block device (queue base addresses,
    queue_ready, device status),
  * the vring (descriptors, avail.idx, used.idx) at the addresses the device
    was actually given,
  * the request header, the status byte and the scratch buffer in RAM.

The status byte the device writes is visible in host physical memory no matter
what the driver's polling loop observed, which separates "the device never
completed the request" from "the driver never saw the completion".

Usage: python tools/riscv_blk_probe.py [wait_seconds]
"""

import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
os.chdir(ROOT)

QMP_PORT = 4473
WAIT_DEFAULT = 120.0

KERNEL = "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64"
DISK = "target/images/riscv64gc-unknown-minix/disk.img"
BLK_ELF = "target/riscv64gc-unknown-minix/release/virtio_blk"

# Driver-server symbol VAs are resolved from the ELF at run time: the
# driver's VA->PA translation is phys_code_base - code_start, where code_start
# is the page-aligned ELF load base, so stale addresses silently
# mis-translate everything.
RING_AVAIL_OFF = 0x1000
RING_USED_OFF = 0x2000

MMIO_BASE = 0x10001000
MMIO_STRIDE = 0x1000
MMIO_SLOTS = 8

REGS = {
    0x000: "magic",
    0x004: "version",
    0x008: "device_id",
    0x00C: "vendor_id",
    0x030: "queue_sel",
    0x034: "queue_num_max",
    0x038: "queue_num",
    0x044: "queue_ready",
    0x070: "status",
    0x080: "queue_desc_low",
    0x084: "queue_desc_high",
    0x090: "queue_avail_low",
    0x094: "queue_avail_high",
    0x0A0: "queue_used_low",
    0x0A4: "queue_used_high",
}

QEMU = [
    "qemu-system-riscv64", "-machine", "virt", "-m", "256M",
    "-nographic", "-monitor", "none",
    "-global", "virtio-mmio.force-legacy=off",
    "-drive", f"if=none,id=disk0,file={DISK},format=raw,cache=writethrough",
    "-device", "virtio-blk-device,drive=disk0",
    "-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0",
    "-device", "virtio-gpu-device", "-device", "virtio-keyboard-device",
    "-qmp", f"tcp:127.0.0.1:{QMP_PORT},server=on,wait=off",
    "-d", "guest_errors",
    "-kernel", KERNEL,
]

lock = threading.Lock()
out = bytearray()
qmp_buf = b""


def pump():
    while True:
        chunk = qemu.stdout.read1(65536)
        if not chunk:
            break
        with lock:
            out.extend(chunk)


def guest_output():
    with lock:
        return bytes(out).decode(errors="replace")


def qmp_cmd(s, obj):
    """Send `obj` and return the next QMP response, skipping the greeting
    and asynchronous events. `None` means the socket closed."""
    global qmp_buf
    if obj is not None:
        s.sendall((json.dumps(obj) + "\n").encode())
    while True:
        while b"\n" in qmp_buf:
            line, qmp_buf = qmp_buf.split(b"\n", 1)
            if not line.strip():
                continue
            msg = json.loads(line)
            if isinstance(msg, dict) and ("return" in msg or "error" in msg):
                return msg
        chunk = s.recv(65536)
        if not chunk:
            return None
        qmp_buf += chunk


def hmp(s, command_line):
    r = qmp_cmd(s, {"execute": "human-monitor-command",
                    "arguments": {"command-line": command_line}})
    if r is None:
        raise RuntimeError("no QMP response for %r" % command_line)
    if "return" not in r:
        raise RuntimeError("QMP error for %r: %r" % (command_line, r))
    ret = r["return"]
    if not isinstance(ret, str):
        raise RuntimeError("unexpected QMP return for %r: %r" % (command_line, ret))
    return ret


def xp(s, width, pa, count):
    """Read `count` items of `width` ('b','w','l','g') at physical `pa`."""
    text = hmp(s, "xp /%d%s 0x%x" % (count, width, pa))
    vals = [int(v, 16) for v in re.findall(r"0x([0-9a-fA-F]+)", text)]
    if len(vals) < count:
        raise RuntimeError("xp /%d%s 0x%x returned %r" % (count, width, pa, text))
    return vals[:count]


def read_bytes(s, pa, size):
    """Read `size` bytes at physical `pa` as a bytearray."""
    words = (size + 3) // 4
    vals = xp(s, "w", pa & ~3, words)
    blob = bytearray()
    for v in vals:
        blob += struct.pack("<I", v)
    off = pa & 3
    return blob[off:off + size]


def llvm_nm():
    for cand in ("llvm-nm", "llvm-nm.exe",
                 r"C:\Program Files\LLVM\bin\llvm-nm.exe"):
        if shutil.which(cand) or os.path.exists(cand):
            return cand
    raise SystemExit("llvm-nm not found")


def elf_symbols(path):
    out = subprocess.run([llvm_nm(), "-S", path], capture_output=True,
                         text=True).stdout
    syms = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[2] in ("b", "B", "d", "D"):
            syms[parts[3]] = int(parts[0], 16)
    return syms


def resolve_va(syms, marker):
    hits = {n: a for n, a in syms.items() if marker in n}
    if len(hits) != 1:
        raise SystemExit("symbol %r matched %d entries: %s"
                         % (marker, len(hits), sorted(hits)))
    return next(iter(hits.values()))


def elf_load_base(path):
    """Page-aligned base vaddr of the ELF's PT_LOAD segments."""
    with open(path, "rb") as f:
        data = f.read()
    e_phoff, = struct.unpack_from("<Q", data, 0x20)
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 0x36)
    base = None
    for i in range(e_phnum):
        p = e_phoff + i * e_phentsize
        p_type, = struct.unpack_from("<I", data, p)
        p_vaddr, p_paddr, p_filesz, p_memsz = struct.unpack_from("<QQQQ", data, p + 16)
        if p_type == 1:
            if base is None or p_vaddr < base:
                base = p_vaddr
    return base & ~0xFFF


wait_s = float(sys.argv[1]) if len(sys.argv) > 1 else WAIT_DEFAULT

print("elf load base: 0x%x" % elf_load_base(BLK_ELF), flush=True)

SYMS = elf_symbols(BLK_ELF)
VA_STATE = resolve_va(SYMS, "10virtio_blk5STATE")
VA_SCRATCH = resolve_va(SYMS, "10virtio_blk7SCRATCH")
VA_PHYS_DELTA = resolve_va(SYMS, "10PHYS_DELTA")
VA_Q0_RING = resolve_va(SYMS, "virtio7Q0_RING")
VA_HDRS = resolve_va(SYMS, "10virtio_blk4HDRS")
VA_STATUS = resolve_va(SYMS, "10virtio_blk6STATUS")
print("symbols: STATE=0x%x SCRATCH=0x%x PHYS_DELTA=0x%x Q0_RING=0x%x "
      "HDRS=0x%x STATUS=0x%x"
      % (VA_STATE, VA_SCRATCH, VA_PHYS_DELTA, VA_Q0_RING, VA_HDRS, VA_STATUS),
      flush=True)

qemu = subprocess.Popen(QEMU, stdin=subprocess.DEVNULL,
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
threading.Thread(target=pump, daemon=True).start()

q = None
deadline = time.time() + 30
while time.time() < deadline and q is None:
    try:
        q = socket.create_connection(("127.0.0.1", QMP_PORT), timeout=5)
    except OSError:
        time.sleep(0.5)
if q is None:
    qemu.kill()
    sys.exit("could not connect to QMP")

qmp_cmd(q, {"execute": "qmp_capabilities"})

# Wait for the boot to reach the block-device failure, then let the state
# settle. Falls back to the plain timeout on a build without markers.
deadline = time.time() + wait_s
while time.time() < deadline:
    text = guest_output()
    if "read failed" in text or "probe FAILED" in text:
        break
    if "init: starting shell" in text and time.time() > deadline - wait_s / 2:
        break
    time.sleep(1.0)
time.sleep(5.0)

report = guest_output()

print("\n=== boot log ===")
for line in report.splitlines():
    if any(k in line for k in ("virtio_blk", "VBLKDBG", "BDEVDBG", "MFSDBG",
                               "VFSDBG", "init:", "BOOT TEST", "FAIL")):
        print("GUEST:", line.strip(), flush=True)

print("\n=== qemu device diagnostics ===")
for line in report.splitlines():
    if any(k in line for k in ("virtio_mmio", "bad offset", "qemu-system-riscv64",
                               "Invalid opcode", "guest_error")):
        print("QEMU:", line.strip(), flush=True)

m = re.search(r"virtio_blk: loaded phys=0x([0-9a-f]+)", report)
delta = None
if not m:
    print("!! no virtio_blk load line in the serial log")
    base = None
else:
    phys = int(m.group(1), 16)
    base = elf_load_base(BLK_ELF)
    delta = phys - base
    print("\nphys_code_base   = 0x%x" % phys)
    print("code_start       = 0x%x" % base)
    print("delta (pa - va)  = 0x%x" % delta)

    got = xp(q, "g", VA_PHYS_DELTA + delta, 1)[0]
    print("PHYS_DELTA static= 0x%x   %s" % (got, "MATCH" if got == delta else "MISMATCH"))

print("\n=== virtio-mmio slots ===")
blk = None
for n in range(MMIO_SLOTS):
    slot = MMIO_BASE + n * MMIO_STRIDE
    magic = xp(q, "w", slot + 0x000, 1)[0]
    if magic != 0x74726976:
        continue
    version = xp(q, "w", slot + 0x004, 1)[0]
    device = xp(q, "w", slot + 0x008, 1)[0]
    vendor = xp(q, "w", slot + 0x00C, 1)[0]
    print("slot %d @0x%x: version=%d device_id=%d vendor=0x%x"
          % (n, slot, version, device, vendor), flush=True)
    if device == 2 and blk is None:
        blk = (n, slot, version)

if blk is None:
    print("!! no virtio-blk mmio slot found")
    qemu.kill()
    sys.exit(1)

n, slot, version = blk
print("\n=== virtio-blk registers @0x%x (slot %d, version %d) ===" % (slot, n, version))
vals = {}
for off, name in sorted(REGS.items()):
    vals[name] = xp(q, "w", slot + off, 1)[0]
    print("  +0x%03x %-16s = 0x%08x" % (off, name, vals[name]))
print("  (QUEUE_SEL/NUM/DESC/AVAIL/USED are write-only in QEMU: reads return 0.\n"
      "   QUEUE_READY=1 plus a non-zero QUEUE_NUM_MAX mean the queue was enabled.)")

if delta is None:
    print("!! cannot locate driver RAM without the boot log")
    qemu.kill()
    sys.exit(1)

ring_pa = VA_Q0_RING + delta
print("\n=== vring @0x%x (Q0_RING va 0x%x + delta) ===" % (ring_pa, VA_Q0_RING))
for i in range(4):
    addr, length, flags, nxt = struct.unpack("<QIHH", bytes(read_bytes(q, ring_pa + i * 16, 16)))
    print("  desc[%d] addr=0x%016x len=0x%x flags=0x%x next=%d" % (i, addr, length, flags, nxt))
    if i == 0 and addr:
        try:
            t, ioprio, sector = struct.unpack("<IIQ", bytes(read_bytes(q, addr, 16)))
            print("           -> outhdr type=%d ioprio=%d sector=%d (%s)"
                  % (t, ioprio, sector,
                     "in the driver image" if addr >> 32 == (phys + 0x1000000) >> 32 else "WRONG REGION"))
        except RuntimeError as e:
            print("           -> outhdr unreadable: %s" % e)

aflags, aidx = struct.unpack("<HH", bytes(read_bytes(q, ring_pa + RING_AVAIL_OFF, 4)))
aring0, = struct.unpack("<H", bytes(read_bytes(q, ring_pa + RING_AVAIL_OFF + 4, 2)))
print("  avail.flags=0x%x avail.idx=%d ring[0]=%d" % (aflags, aidx, aring0))

uflags, uidx = struct.unpack("<HH", bytes(read_bytes(q, ring_pa + RING_USED_OFF, 4)))
uid, ulen = struct.unpack("<II", bytes(read_bytes(q, ring_pa + RING_USED_OFF + 4, 8)))
print("  used.flags=0x%x used.idx=%d ring[0]: id=%d len=%d" % (uflags, uidx, uid, ulen))

print("\n=== driver state (STATE @0x%x) ===" % VA_STATE)
state = read_bytes(q, VA_STATE + delta, 0xF8)
for off in range(0, len(state), 8):
    v, = struct.unpack("<Q", bytes(state[off:off + 8]))
    if v:
        print("  STATE+0x%02x = 0x%016x" % (off, v))

if delta is not None:
    st = read_bytes(q, VA_STATUS + delta, 1)[0]
    print("\n=== driver DMA buffers (pa = va + 0x%x) ===" % delta)
    around = bytes(read_bytes(q, VA_STATUS + delta - 1, 3))
    print("  STATUS         = 0x%02x  %s"
          % (st, "S_OK (device completed!)" if st == 0 else "NOT completed"))
    print("  pa STATUS-1..+1= %s   (va 0x%x-1..+1)" % (around.hex(), VA_STATUS))
    hdr_buf = read_bytes(q, VA_HDRS + delta, 16)
    print("  outhdr @HDRS   = %s" % bytes(hdr_buf).hex())
    scr = bytes(read_bytes(q, VA_SCRATCH + delta, 32))
    print("  SCRATCH[0:32]  = %s" % scr.hex())
    scr_mid = bytes(read_bytes(q, VA_SCRATCH + delta + 1024, 16))
    img = open(DISK, "rb").read(4096)
    print("  SCRATCH[1024:1040] = %s" % scr_mid.hex())
    print("  disk[0:32]     = %s" % img[:32].hex())
    print("  disk[1024:1040]= %s" % img[1024:1040].hex())
    print("  disk DMA match = %s" % (scr_mid == img[1024:1040]))
    st_msgs = [ln for ln in report.splitlines() if "VBLKDBG" in ln or "BDEVDBG" in ln]
    if st_msgs:
        print("  driver said: %s" % st_msgs[0].strip())
        print("  driver said: %s" % st_msgs[-1].strip())

q.close()
qemu.kill()
print("\n=== serial tail ===")
tail = report.splitlines()[-12:]
for line in tail:
    print("  " + line)
