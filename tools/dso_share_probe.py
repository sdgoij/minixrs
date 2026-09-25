#!/usr/bin/env python3
"""Measure whether two processes mapping the same shared object share its frames.

Phase 5 of `DYNAMIC_LINKING.md`. VM's file page cache is what makes a read-only
DSO page one physical frame for every process that maps it, and that only holds if
the loader maps the object's read-only segments *read-only* — a loader that maps
them writable so it can patch them in place gets a private copy of every page. The
doc's gate asks for a measured assertion rather than a comment, so this measures:

  boot the dynamic-linking image, run `/bin/dynclib hold | /bin/dynclib hold` (the
  shell's own fork and exec, so two lives are running the same dynamic image with
  `libc.so` mapped by the loader), then walk *both* processes' page tables from
  outside the guest and compare the physical frames behind the object's pages.

The two lives must overlap, and a pipeline is how the shell runs two programs at
once. (`/bin/dynclib pair`, which forked and re-exec'd itself, was tried first: the
child's `execv` never returned — worth a look separately, and not needed here.)

Both processes map the object at the same virtual address: the loader's base
allocator is deterministic (`crates/ldso/src/layout.rs::DSO_BASE`) and the port
has no ASLR. So "shared" is the same VA in two address spaces resolving to one
frame.

Why not ask the guest: the loader is the thing under test, so a guest-side
measurement would have the subject measure itself; and only the kernel knows a
virtual address's frame. The guest's part is only to *stay alive* (the `hold`
mode) while the host reads the page tables out of guest physical memory.

Self-validating on purpose: nothing is identified by process name, and the kernel's
`Proc` layout is read out of the image being booted rather than hardcoded — an
earlier version of this probe carried a table address from an older image and found
nobody mapping anything at all. Every slot whose page-table root maps the object is
a candidate, and the run fails unless exactly two are found *and* the same walk
first sees the program text that several processes share. A wrong table address or
stride therefore cannot pass quietly: it finds nothing, and says so.

The walk is x86_64's, as is the loader it measures: `crates/ldso` has no other arch
yet. So Phase 5 is measured here, and the second arch its gate asks for is inherited
by Phase 7, which ports the loader (`DYNAMIC_LINKING.md` §7). Porting this needs an
arch parameter for `frames_in` and for the `fpu_state`-to-`p_cr3` derivation below
(that field is an inline area on riscv64, not a pointer), and `satp` in place of
CR3.

As first run it reports FAIL, and the failure is real: the object's read-only pages
are mapped read-only (the loader's discipline holds) but private, and the `control`
line shows the sharing absent below the loader too. `DYNAMIC_LINKING.md` §7 Phase 5
has the measurement and the cause; the tool's job is to keep that verdict honest.

Usage: python tools/dso_share_probe.py [MEM] [DYNCLIB_PATH]
"""

import json
import pathlib
import socket
import struct
import subprocess
import sys
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from lld import find_nm  # noqa: E402

MEM = sys.argv[1] if len(sys.argv) > 1 else "256M"
DYNCLIB = sys.argv[2] if len(sys.argv) > 2 else "/bin/dynclib"

# The process table and the `Proc` layout are read from the kernel image, not
# hardcoded: the first version of this probe carried the table's address from an
# older image and found nobody mapping anything at all. `AlignedTable` holds
# `NR_PROCS + 5` slots (crates/arch-common/src/consts.rs, crates/kernel/src/table.rs).
KERNEL_ELF = ROOT / "target/x86_64-pc-minix/release/kernel-boot"
SLOTS = 256 + 5

# Where the loader maps its first object (crates/ldso/src/layout.rs).
DSO_BASE = 0x0200_0000
# Walk the object's first 256 KiB: the loader's mapping starts at DSO_BASE and no
# DSO here is larger, so a present page in this range is one of the object's.
DSO_WINDOW = 0x40000
# Pages the two processes must *both* have faulted in, to compare. Below this the
# run proves too little to report.
MIN_COMPARED = 8
# Every minix program is linked at this base (tools/minix-user.ld), so a walk that
# finds nobody here is a broken walk rather than a system with nothing in it.
TEXT_BASE = 0x0100_0000
TEXT_WINDOW = 0x4000
MIN_TEXT_MAPPERS = 5

PG_P = 0x001
PG_RW = 0x002
PG_PS = 0x080
PG_FRAME = 0x000FFFFFFFFFF000

PROMPT = b"# "
HOLD_MARKER = b"dynclib-hold"
# How many processes have to have the object mapped for the comparison to mean
# anything: the two sides of the pipeline.
NEED_MAPPERS = 2

# Typing pace. The console drops bytes that arrive faster than it drains them.
CHUNK_BYTES = 6
CHUNK_PAUSE = 0.02

HOLD_COMMAND = f"{DYNCLIB} hold | {DYNCLIB} hold"

qemu = subprocess.Popen(
    [
        "qemu-system-x86_64", "-nographic", "-monitor", "none",
        "-qmp", "tcp:127.0.0.1:4444,server,nowait",
        "-m", MEM, "-no-reboot",
        "-vga", "none", "-device", "bochs-display,id=fb0",
        "-netdev", "user,id=net0",
        "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
        "-device", "virtio-tablet-pci,display=fb0",
        "-kernel", str(ROOT / "target/images/x86_64-pc-minix/minix-x86.elf"),
    ],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
)

lock = threading.Lock()
out = bytearray()
qemu_stdin = qemu.stdin


def pump() -> None:
    while True:
        c = qemu.stdout.read1(65536)
        if not c:
            break
        with lock:
            out.extend(c)


threading.Thread(target=pump, daemon=True).start()


def seen(needle: bytes) -> bool:
    with lock:
        return needle in bytes(out)


def wait_for(needle: bytes, timeout: float) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if seen(needle):
            return True
        time.sleep(0.05)
    return seen(needle)


def send_raw(data: bytes) -> None:
    qemu_stdin.write(data)
    qemu_stdin.flush()


def send_command(command: str) -> bool:
    """Type one command, paced, and check the guest echoed the whole of it.

    The console drops what it is not ready for — the trap `tools/smoke/feed.sh` is
    shaped around — and it does so *mid-line*, not just at the tail: measured
    here, `/bin/dynclib hold | /bin/dynclib hold` (37 bytes) arrived as
    `/bin/dynclib hold | /biniold` with one burst and as `/bin/dynclib hol |
    /bin/dyncli hold` with another. A truncated line still runs, as a shorter
    command, so sending is not evidence that the command ran. The echo is, checked
    whole: the guest echoes exactly what it was given.
    """
    echo = b"# " + command.encode()
    for _ in range(4):
        time.sleep(0.4)
        for i in range(0, len(command), CHUNK_BYTES):
            send_raw(command[i:i + CHUNK_BYTES].encode())
            time.sleep(CHUNK_PAUSE)
        send_raw(b"\n")
        if wait_for(echo, 10):
            return True
        # Something shorter ran. Let the shell come back before trying again, or
        # the retry lands in the same window.
        wait_for(PROMPT, 10)
    return False


def fail(message: str) -> int:
    """Report and show what the guest said. A probe that fails without the
    console is one that has to be re-run to be understood."""
    print(f"error: {message}", file=sys.stderr)
    with lock:
        tail = bytes(out)[-2000:]
    print("--- guest console (tail) ---", file=sys.stderr)
    sys.stderr.write(tail.decode("utf-8", "replace"))
    print("\n--- end ---", file=sys.stderr)
    return 1


def read_phys(phys: int, size: int) -> bytes:
    """Dump guest physical memory over QMP. `pmemsave` is the only way in."""
    path = ROOT / "target/dso_share.bin"
    q = socket.create_connection(("127.0.0.1", 4444), timeout=10)
    q.settimeout(20)
    f = q.makefile("rb")
    f.readline()
    q.sendall(b'{"execute":"qmp_capabilities"}\n')
    f.readline()
    q.sendall((json.dumps({
        "execute": "pmemsave",
        "arguments": {"val": phys, "size": size, "filename": str(path)},
    }) + "\n").encode())
    reply = json.loads(f.readline().decode())
    q.close()
    if "error" in reply:
        sys.exit(f"error: QMP pmemsave failed: {reply['error']}")
    return path.read_bytes()


def entry(table: int, index: int) -> int:
    """One page-table entry out of guest physical memory."""
    return struct.unpack_from("<Q", read_phys(table + index * 8, 8))[0]


def leaf_table(cr3: int, base: int) -> "int | None":
    """Physical address of the 4 KiB page table covering `base`, or None.

    One table page per level: PML4, PDPT, PD, and then the PT itself.
    """
    e4 = entry(cr3, (base >> 39) & 0x1FF)
    if not e4 & PG_P or e4 & PG_PS:
        return None
    e3 = entry(e4 & PG_FRAME, (base >> 30) & 0x1FF)
    if not e3 & PG_P or e3 & PG_PS:
        return None
    e2 = entry(e3 & PG_FRAME, (base >> 21) & 0x1FF)
    if not e2 & PG_P or e2 & PG_PS:
        return None
    return e2 & PG_FRAME


def leaf(cr3: int, va: int) -> int:
    """The raw 4 KiB page-table entry for `va` in `cr3`, or 0 when unmapped."""
    pt = leaf_table(cr3, va)
    if pt is None:
        return 0
    return struct.unpack_from("<Q", read_phys(pt + ((va >> 12) & 0x1FF) * 8, 8))[0]


def frames_in(cr3: int, base: int, window: int) -> "dict[int, int]":
    """Every present page of a mapping in this address space, as `va -> frame`.

    The window lies inside one 2 MiB region, so the walk needs three single-entry
    reads and one whole page, per process.
    """
    assert (base >> 21) == ((base + window - 1) >> 21), \
        "the window must not span a 2 MiB region"

    pt_frame = leaf_table(cr3, base)
    if pt_frame is None:
        return {}
    pt = read_phys(pt_frame, 4096)
    found = {}
    for off in range(0, window, 4096):
        idx = ((base + off) >> 12) & 0x1FF
        e1 = struct.unpack_from("<Q", pt, idx * 8)[0]
        if e1 & PG_P:
            found[base + off] = e1 & PG_FRAME
    return found


def symbol_places(names: list[str]) -> "dict[str, tuple[int, int]]":
    """Address and size of each kernel symbol, read out of the image."""
    nm = find_nm()
    if nm is None:
        sys.exit("error: no llvm-nm to read the kernel image's symbols"
                 " (set MINIXRS_LLVM_NM, or install LLVM)")
    out = subprocess.run([str(nm), "--print-size", str(KERNEL_ELF)],
                         capture_output=True, text=True).stdout
    placed: dict[str, tuple[int, int]] = {}
    for line in out.splitlines():
        f = line.split()
        if len(f) < 4:
            continue
        for name in names:
            if f[-1].endswith(name):
                placed[name] = (int(f[0], 16), int(f[1], 16))
    missing = [n for n in names if n not in placed]
    if missing:
        sys.exit(f"error: {KERNEL_ELF.name} has no symbol for {', '.join(missing)}")
    return placed


def layout() -> "tuple[int, int, int]":
    """`(table, stride, p_cr3)` for the running kernel.

    `p_cr3` is `offset_of(Proc, p_seg)`: `SegFrame` starts with `p_cr3`
    (`#[repr(C)]`, crates/kernel/src/proc.rs) and the kernel publishes that offset
    itself at boot — `FPU_STATE_OFF` is `p_seg` plus `fpu_state`'s own offset, 16
    (crates/kernel/src/fpu.rs). A wrong answer here cannot pass quietly: the walk
    below has to see the program text before it believes anything it reads.
    """
    if not KERNEL_ELF.is_file():
        sys.exit(f"error: no kernel image at {KERNEL_ELF} - build it first")
    places = symbol_places(["PROC_TABLE_ALIGNED", "FPU_STATE_OFF"])
    table, table_size = places["PROC_TABLE_ALIGNED"]
    stride = table_size // SLOTS
    fpu_off = int.from_bytes(read_phys(places["FPU_STATE_OFF"][0], 8), "little")
    if stride == 0 or fpu_off < 16:
        sys.exit(f"error: the kernel image's layout does not make sense"
                 f" (table size {table_size}, FPU_STATE_OFF {fpu_off})")
    return table, stride, fpu_off - 16


def slot_roots(place: "tuple[int, int, int]") -> "list[int]":
    """Every slot's page-table root, out of one dump of the whole table.

    One `pmemsave` rather than one per slot: the table is a contiguous array and
    the slot record is a fixed stride, so the roots are strided fields of a single
    read. Taken *after* the pipeline has started — a slot's root is zero while the
    slot is free, so a snapshot from before the two lives existed would find
    nothing to compare.
    """
    table, stride, p_cr3 = place
    dump = read_phys(table, SLOTS * stride)
    return [int.from_bytes(dump[s * stride + p_cr3:s * stride + p_cr3 + 8], "little")
            for s in range(SLOTS)]


def mappers(roots: "list[int]", base: int,
            window: int) -> "dict[int, dict[int, int]]":
    """Every live slot that maps `base`, as `slot -> {va: frame}`."""
    found = {}
    for slot, cr3 in enumerate(roots):
        if cr3 == 0:
            continue
        frames = frames_in(cr3, base, window)
        if frames:
            found[slot] = frames
    return found


def report_pages(roots: "list[int]", fa: "dict[int, int]", fb: "dict[int, int]",
                 sa: int, sb: int, vas: "list[int]") -> None:
    """Per-page frames and PTE flags, which is what makes a failure diagnosable.

    A read-only PTE that is still private means the page was never shared (a cache
    miss); a writable one means the region was mapped writable, which is a mapping
    discipline problem instead.
    """
    for va in vas:
        ea, eb = leaf(roots[sa], va), leaf(roots[sb], va)
        print(f"  0x{va:x}: {'SAME' if fa.get(va) == fb.get(va) else 'diff'}"
              f" frames 0x{fa.get(va, 0):x}/0x{fb.get(va, 0):x}"
              f" RW={bool(ea & PG_RW)}/{bool(eb & PG_RW)}", flush=True)


def main() -> int:
    try:
        if not wait_for(PROMPT, 60):
            return fail("the guest never reached a shell prompt")

        place = layout()
        print(f"proc table 0x{place[0]:x}, slot stride {place[1]}, p_cr3 at"
              f" +{place[2]}", flush=True)

        if not send_command(HOLD_COMMAND):
            return fail(f"the guest never echoed the whole of '{HOLD_COMMAND}'")

        # One marker per process of the pipeline, each printed after the loader
        # has run and `crt0` has entered `main`.
        deadline = time.time() + 30
        while time.time() < deadline:
            with lock:
                if bytes(out).count(HOLD_MARKER) >= NEED_MAPPERS:
                    break
            time.sleep(0.05)
        else:
            return fail(f"fewer than {NEED_MAPPERS} '{HOLD_MARKER.decode()}' "
                        "lines - the pipeline never started")
        time.sleep(0.5)

        roots = slot_roots(place)
        live = sum(1 for cr3 in roots if cr3)
        print(f"{live} slot(s) hold a page-table root", flush=True)

        # Before trusting the walk, check it can see something known: every minix
        # program is linked at TEXT_BASE, so a live system has several processes
        # there. A walk that finds none is a broken instrument, not a finding.
        text = mappers(roots, TEXT_BASE, TEXT_WINDOW)
        if len(text) < MIN_TEXT_MAPPERS:
            return fail(f"only {len(text)} process(es) map the program text at "
                        f"0x{TEXT_BASE:x} - the page-table walk is not working, so"
                        " it cannot be believed about the object either")

        mapper = mappers(roots, DSO_BASE, DSO_WINDOW)
        if len(mapper) != NEED_MAPPERS:
            return fail(f"{len(mapper)} process(es) map the object at "
                        f"0x{DSO_BASE:x}, expected {NEED_MAPPERS} "
                        f"(slots {sorted(mapper)})")

        (sa, a), (sb, b) = sorted(mapper.items())

        # The control, without which a "differ" verdict cannot be read: the same two
        # processes' program text is mapped by exec, not by the loader. If the object's
        # pages differ while these are one frame, the fault is in the loader's mapping;
        # if the control differs too, the sharing is absent below the loader — VM's
        # file-page cache — and the loader would be the wrong thing to change.
        ta, tb = text.get(sa), text.get(sb)
        if ta and tb:
            common = sorted(set(ta) & set(tb))
            same = sum(1 for va in common if ta[va] == tb[va])
            print(f"control: the same two processes share {same}/{len(common)} "
                  f"program-text page(s) at 0x{TEXT_BASE:x}", flush=True)
            if same != len(common):
                report_pages(roots, ta, tb, sa, sb, common)

        shared = sorted(set(a) & set(b))
        if len(shared) < MIN_COMPARED:
            return fail(f"only {len(shared)} page(s) present in both processes "
                        f"(need {MIN_COMPARED}); slot {sa} has {len(a)}, "
                        f"slot {sb} has {len(b)}")

        mismatched = [va for va in shared if a[va] != b[va]]
        pages = len(shared)
        if mismatched:
            va = mismatched[0]
            pa, pb = leaf(roots[sa], va), leaf(roots[sb], va)
            print(f"FAIL dso-share: {len(mismatched)} of {pages} shared page(s) differ;"
                  f" first at 0x{va:x}: frames 0x{a[va]:x} vs 0x{b[va]:x},"
                  f" PTEs 0x{pa:x} (RW={bool(pa & PG_RW)}) vs 0x{pb:x}"
                  f" (RW={bool(pb & PG_RW)})")
            report_pages(roots, a, b, sa, sb, sorted(set(a) | set(b)))
            return 1

        frame = a[shared[0]]
        print(f"PASS dso-share: {pages} page(s) of the object at 0x{DSO_BASE:x} are one"
              f" frame set in two processes (slots {sa} and {sb}; first page frame"
              f" 0x{frame:x})")
        return 0
    finally:
        qemu.kill()


if __name__ == "__main__":
    sys.exit(main())
