#!/usr/bin/env python3
"""Measure whether two processes mapping the same shared object share its frames.

Phase 5 of `DYNAMIC_LINKING.md`. VM's file page cache is what makes a read-only
DSO page one physical frame for every process that maps it, and that only holds if
the loader maps the object's read-only segments *read-only* — a loader that maps
them writable so it can patch them in place gets a private copy of every page. The
doc's gate asks for a measured assertion rather than a comment, so this measures:

  boot the dynamic-linking image, run `/bin/dynclib hold | /bin/dynclib hold` (the shell's
  own fork and exec, so two lives are running the same dynamic image with `libc.so`
  mapped by the loader), and walk *both* processes' page tables from outside the guest,
  comparing the physical frames behind the object's read-only pages.

Both lives start together, which is the hard case and the one the shell, `init` and any
build job produce: the two fill the same pages at the same time. VM joins a fill already
in flight, so the second process waits for the first's page and maps that frame rather
than filling a private copy of its own (`vm/vfs_request.rs::park_page` and
`release_parked`). Without that joining, both miss every page the other is filling and
the measurement fails — which is what it did before the joining landed, and what makes
this probe worth re-running when VM's file fault path changes.

Only read-only pages are compared. Each mapping's writable `.data`/`.got` is *expected*
to be private (that is where the relocations go), so counting those as failures would
fail every run; the report says how many were seen instead.

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

The walk is the arch's own, chosen with `--arch`. Only the walk differs: the loader's
`DSO_BASE` and every program's `TEXT_BASE` are shared, so which levels to descend, the
bits that make an entry present, a huge page or writable, and the bits holding the frame
are a row in `ARCHS` rather than a second copy of this file. aarch64 is not a row yet: its
read-only bit (`AP[2]`) is inverted relative to these two, so it is its own change and not
a data entry. Phase 5's gate asks for two arches; x86_64 and riscv64 are the two.

As first run it reported FAIL and invited a wrong conclusion: the pages were private, and
only reading VM's own state showed why (`DYNAMIC_LINKING.md` §7 Phase 5). Its job is to
keep that verdict honest.

Usage: python tools/dso_share_probe.py [--arch x86|riscv64] [MEM] [DYNCLIB_PATH]
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

def parse_args(argv: "list[str]") -> "tuple[str, list[str]]":
    """The `--arch` value and the positionals around it."""
    arch, rest, i = "x86", [], 0
    while i < len(argv):
        if argv[i] == "--arch":
            i += 1
            if i >= len(argv):
                sys.exit("error: --arch needs a value")
            arch = argv[i]
        elif argv[i].startswith("--arch="):
            arch = argv[i].split("=", 1)[1]
        else:
            rest.append(argv[i])
        i += 1
    return arch, rest


IMAGE = ROOT / "target/images/x86_64-pc-minix/minix-x86.elf"
RISCV_IMAGE = ROOT / "target/riscv64gc-unknown-minix/release/kernel-boot-riscv64"

# One row per arch: what to read the process table out of, what to boot, and the bits
# of its page-table walk. `shifts` are the index shifts of the levels *above* the leaf
# table, root first; the leaf table's own entry is index `(va >> 12) & 0x1FF` in every
# arch here. A present non-leaf entry that is actually a leaf (a huge page this walk
# does not follow) differs: x86_64 sets `PG_PS`, riscv64 makes a leaf by setting R or X.
#
# `p_seg_offset` is `offset_of(Proc, p_seg)`, the `p_cr3` field. No arch but x86_64
# publishes it (see `layout`), and kernel assembly hardcodes the same number
# (crates/kernel/src/proc.rs documents x86 +256, riscv +264, aarch64 +288), so the
# other arches carry it here; the `TEXT_BASE` control below is what makes a wrong
# value fail loudly instead of quietly finding nothing.
ARCHS = {
    "x86": {
        "kernel_elf": ROOT / "target/x86_64-pc-minix/release/kernel-boot",
        "image": IMAGE,
        # Typing pace: `(bytes per write, pause between writes)`. The console drops
        # bytes it is not ready for (see `send_command`), and how many it keeps per
        # write is the arch's own. Measured on the x86_64 image: 6 bytes at a time is
        # safe. Measured on the riscv64 image: only *one* byte of a write survives, so
        # writing 2 at a time loses the second (a 6-byte write kept about one byte in
        # six) — hence one byte per write there. Both were measured with
        # `echo 0123456789abcdefghijklmnopqrst`, 37 distinct bytes like the command.
        "pace": (6, 0.02),
        "qemu": lambda memory: [
            "qemu-system-x86_64", "-nographic", "-monitor", "none",
            "-qmp", "tcp:127.0.0.1:4444,server,nowait",
            "-m", memory, "-no-reboot",
            "-vga", "none", "-device", "bochs-display,id=fb0",
            "-netdev", "user,id=net0",
            "-device", "virtio-net-pci,disable-legacy=on,netdev=net0",
            "-device", "virtio-tablet-pci,display=fb0",
            "-kernel", str(IMAGE),
        ],
        "shifts": (39, 30, 21),
        "pg_present": 0x001,
        "pg_huge": 0x080,
        "pg_writable": 0x002,
        "frame": lambda e: e & 0x000FFFFFFFFFF000,
        "p_seg_offset": None,
    },
    "riscv64": {
        "kernel_elf": RISCV_IMAGE,
        "image": RISCV_IMAGE,
        "pace": (1, 0.02),
        "qemu": lambda memory: [
            "qemu-system-riscv64", "-machine", "virt", "-m", memory, "-nographic",
            "-monitor", "none", "-qmp", "tcp:127.0.0.1:4444,server,nowait",
            "-global", "virtio-mmio.force-legacy=off",
            "-netdev", "user,id=net0",
            "-device", "virtio-net-device,netdev=net0",
            "-device", "virtio-gpu-device",
            "-device", "virtio-keyboard-device",
            "-kernel", str(RISCV_IMAGE),
        ],
        "shifts": (30, 21),
        "pg_present": 0x001,
        "pg_huge": 0x00A,
        "pg_writable": 0x004,
        "frame": lambda e: ((e >> 10) & 0xFFFFFFFFFFF) << 12,
        "p_seg_offset": 264,
    },
}

ARCH_NAME, ARGS = parse_args(sys.argv[1:])
if ARCH_NAME not in ARCHS:
    sys.exit(f"error: unknown --arch {ARCH_NAME} (have {', '.join(ARCHS)})")
PAGING = ARCHS[ARCH_NAME]

MEM = ARGS[0] if len(ARGS) > 0 else "256M"
DYNCLIB = ARGS[1] if len(ARGS) > 1 else "/bin/dynclib"

# The process table is read from the kernel image, not hardcoded: the first version of
# this probe carried the table's address from an older image and found nobody mapping
# anything at all. `AlignedTable` holds `NR_PROCS + 5` slots
# (crates/arch-common/src/consts.rs, crates/kernel/src/table.rs).
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

PROMPT = b"# "
HOLD_MARKER = b"dynclib-hold"
# How many processes have to have the object mapped for the comparison to mean
# anything: the two sides of the pipeline.
NEED_MAPPERS = 2

# Typing pace lives with the arch in `ARCHS`: how many bytes of a write the console
# keeps is not the same on every one (see the comment on x86 there).

HOLD_COMMAND = f"{DYNCLIB} hold | {DYNCLIB} hold"

qemu = subprocess.Popen(
    PAGING["qemu"](MEM),
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


def wait_for_after(needle: bytes, start: int, timeout: float) -> bool:
    """Like `wait_for`, but only the part printed since `start` counts.

    A stale match is not a wait: the prompt from before a command was typed is still
    in the buffer, so scanning the whole of it finds a prompt that says nothing about
    whether the shell has come back.
    """
    deadline = time.time() + timeout
    while True:
        with lock:
            if needle in bytes(out)[start:]:
                return True
        if time.time() >= deadline:
            return False
        time.sleep(0.05)


def send_raw(data: bytes) -> None:
    qemu_stdin.write(data)
    qemu_stdin.flush()


def send_command(command: str) -> bool:
    """Type one command, paced, and check the guest echoed the whole of it.

    The console drops what it is not ready for — the trap `tools/smoke/feed.sh` is
    shaped around — and it does so *mid-line*, not just at the tail: measured on the
    x86_64 image, `/bin/dynclib hold | /bin/dynclib hold` (37 bytes) arrived as
    `/bin/dynclib hold | /biniold` with one burst and as `/bin/dynclib hol |
    /bin/dyncli hold` with another. A truncated line still runs, as a shorter
    command, so sending is not evidence that the command ran. The echo is, checked
    whole: the guest echoes exactly what it was given.

    How many bytes may go in one write is the arch's (`ARCHS["pace"]`): measured on
    the riscv64 image, a write of 2 keeps only its first byte and one of 6 keeps
    about one in six, so that arch writes one byte at a time. The retry waits for a
    *new* prompt, or it would type into the window while the truncated command from
    the previous attempt is still running, and lose the retry too.
    """
    chunk, pause = PAGING["pace"]
    echo = command.encode()
    for _ in range(4):
        time.sleep(0.4)
        start = len(out)
        for i in range(0, len(command), chunk):
            send_raw(command[i:i + chunk].encode())
            time.sleep(pause)
        send_raw(b"\n")
        # The command's own echo, not `"# " + command`: the prompt that precedes it
        # was printed before `start`, so a needle spanning the two is never contiguous
        # in the window. `start` is the anchor instead, and it also keeps a previous
        # attempt's echo out of this one's verdict.
        if wait_for_after(echo, start, 10):
            return True
        # Something shorter ran. Let the shell come back before trying again, or
        # the retry lands in the same window.
        wait_for_after(b"\n# ", start, 10)
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


def leaf_table(paging: dict, root: int, base: int) -> "int | None":
    """Physical address of the leaf page table covering `base`, or None.

    One table page per level above the leaf: PML4, PDPT and PD on x86_64; root and mid
    on riscv64. A level whose entry is present but is itself a leaf (a huge page) is
    not followed — this walk only has 4 KiB pages.
    """
    table = root
    for shift in paging["shifts"]:
        e = entry(table, (base >> shift) & 0x1FF)
        if not e & paging["pg_present"] or e & paging["pg_huge"]:
            return None
        table = paging["frame"](e)
    return table


def leaf(paging: dict, root: int, va: int) -> int:
    """The raw 4 KiB page-table entry for `va` in `root`, or 0 when unmapped."""
    pt = leaf_table(paging, root, va)
    if pt is None:
        return 0
    return struct.unpack_from("<Q", read_phys(pt + ((va >> 12) & 0x1FF) * 8, 8))[0]


def frames_in(paging: dict, root: int, base: int, window: int) -> "dict[int, int]":
    """Every present page of a mapping in this address space, as `va -> frame`.

    The window lies inside one 2 MiB region, so the walk needs a few single-entry
    reads and one whole page, per process.
    """
    assert (base >> 21) == ((base + window - 1) >> 21), \
        "the window must not span a 2 MiB region"

    pt_frame = leaf_table(paging, root, base)
    if pt_frame is None:
        return {}
    pt = read_phys(pt_frame, 4096)
    found = {}
    for off in range(0, window, 4096):
        idx = ((base + off) >> 12) & 0x1FF
        e1 = struct.unpack_from("<Q", pt, idx * 8)[0]
        if e1 & paging["pg_present"]:
            found[base + off] = paging["frame"](e1)
    return found


def symbol_places(names: list[str], elf: pathlib.Path) -> "dict[str, tuple[int, int]]":
    """Address and size of each kernel symbol, read out of the image."""
    nm = find_nm()
    if nm is None:
        sys.exit("error: no llvm-nm to read the kernel image's symbols"
                 " (set MINIXRS_LLVM_NM, or install LLVM)")
    out = subprocess.run([str(nm), "--print-size", str(elf)],
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
        sys.exit(f"error: {elf.name} has no symbol for {', '.join(missing)}")
    return placed


def layout(paging: dict) -> "tuple[int, int, int]":
    """`(table, stride, p_cr3)` for the running kernel.

    `p_cr3` is `offset_of(Proc, p_seg)`: `SegFrame` starts with `p_cr3`
    (`#[repr(C)]`, crates/kernel/src/proc.rs). Only x86_64 publishes that offset —
    `FPU_STATE_OFF` is `p_seg` plus `fpu_state`'s own offset of 16
    (crates/kernel/src/fpu.rs) — so it is read there and taken from `ARCHS`
    elsewhere. A wrong answer here cannot pass quietly: the walk below has to see the
    program text before it believes anything it reads.
    """
    elf = paging["kernel_elf"]
    if not elf.is_file():
        sys.exit(f"error: no kernel image at {elf} - build it first")
    names = ["PROC_TABLE_ALIGNED"]
    if paging["p_seg_offset"] is None:
        names.append("FPU_STATE_OFF")
    places = symbol_places(names, elf)
    table, table_size = places["PROC_TABLE_ALIGNED"]
    stride = table_size // SLOTS
    p_seg = paging["p_seg_offset"]
    if p_seg is None:
        p_seg = int.from_bytes(read_phys(places["FPU_STATE_OFF"][0], 8), "little") - 16
    if stride == 0 or p_seg < 16 or p_seg >= stride:
        sys.exit(f"error: the kernel image's layout does not make sense"
                 f" (table size {table_size}, p_cr3 offset {p_seg})")
    return table, stride, p_seg


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


def mappers(paging: dict, roots: "list[int]", base: int,
            window: int) -> "dict[int, dict[int, int]]":
    """Every live slot that maps `base`, as `slot -> {va: frame}`."""
    found = {}
    for slot, root in enumerate(roots):
        if root == 0:
            continue
        frames = frames_in(paging, root, base, window)
        if frames:
            found[slot] = frames
    return found


def report_pages(paging: dict, roots: "list[int]", fa: "dict[int, int]",
                 fb: "dict[int, int]", sa: int, sb: int,
                 vas: "list[int]") -> None:
    """Per-page frames and PTE flags, which is what makes a failure diagnosable.

    A read-only PTE that is still private means the page was never shared (a cache
    miss); a writable one means the region was mapped writable, which is a mapping
    discipline problem instead.
    """
    for va in vas:
        ea, eb = leaf(paging, roots[sa], va), leaf(paging, roots[sb], va)
        print(f"  0x{va:x}: {'SAME' if fa.get(va) == fb.get(va) else 'diff'}"
              f" frames 0x{fa.get(va, 0):x}/0x{fb.get(va, 0):x}"
              f" RW={bool(ea & paging['pg_writable'])}"
              f"/{bool(eb & paging['pg_writable'])}", flush=True)


def main() -> int:
    try:
        if not wait_for(PROMPT, 60):
            return fail("the guest never reached a shell prompt")

        place = layout(PAGING)
        print(f"dso-share probe ({ARCH_NAME}): proc table 0x{place[0]:x}, slot stride"
              f" {place[1]}, p_cr3 at +{place[2]}", flush=True)

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
        live = sum(1 for root in roots if root)
        print(f"{live} slot(s) hold a page-table root", flush=True)

        # Before trusting the walk, check it can see something known: every minix
        # program is linked at TEXT_BASE, so a live system has several processes
        # there. A walk that finds none is a broken instrument, not a finding.
        text = mappers(PAGING, roots, TEXT_BASE, TEXT_WINDOW)
        if len(text) < MIN_TEXT_MAPPERS:
            return fail(f"only {len(text)} process(es) map the program text at "
                        f"0x{TEXT_BASE:x} - the page-table walk is not working, so"
                        " it cannot be believed about the object either")

        mapper = mappers(PAGING, roots, DSO_BASE, DSO_WINDOW)
        if len(mapper) != NEED_MAPPERS:
            return fail(f"{len(mapper)} process(es) map the object at "
                        f"0x{DSO_BASE:x}, expected {NEED_MAPPERS} "
                        f"(slots {sorted(mapper)})")

        (sa, a), (sb, b) = sorted(mapper.items())

        # The control, without which a "differ" verdict cannot be read: the same two
        # processes' own program text is mapped by exec, not by the loader.
        ta, tb = text.get(sa), text.get(sb)
        if ta and tb:
            common = sorted(set(ta) & set(tb))
            ro = [va for va in common
                  if not leaf(PAGING, roots[sa], va) & PAGING["pg_writable"]]
            same = sum(1 for va in ro if ta[va] == tb[va])
            print(f"control: the same two processes share {same}/{len(ro)} read-only"
                  f" program-text page(s) at 0x{TEXT_BASE:x}", flush=True)
            if same != len(ro):
                report_pages(PAGING, roots, ta, tb, sa, sb, ro)

        # Compare the read-only pages, and only those: the writable part of each
        # mapping is private by design.
        common = sorted(set(a) & set(b))
        rw = [va for va in common if leaf(PAGING, roots[sa], va) & PAGING["pg_writable"]]
        ro = [va for va in common
              if not leaf(PAGING, roots[sa], va) & PAGING["pg_writable"]]
        if len(ro) < MIN_COMPARED:
            return fail(f"only {len(ro)} read-only page(s) are present in both"
                        f" processes (need {MIN_COMPARED}); slot {sa} has {len(a)},"
                        f" slot {sb} has {len(b)}")

        differing = [va for va in ro if a[va] != b[va]]
        if differing:
            va = differing[0]
            pa, pb = leaf(PAGING, roots[sa], va), leaf(PAGING, roots[sb], va)
            rwbit = PAGING["pg_writable"]
            print(f"FAIL dso-share: {len(differing)} of {len(ro)} read-only page(s)"
                  f" differ; first at 0x{va:x}: frames 0x{a[va]:x} vs 0x{b[va]:x},"
                  f" PTEs 0x{pa:x} (RW={bool(pa & rwbit)}) vs 0x{pb:x}"
                  f" (RW={bool(pb & rwbit)})")
            report_pages(PAGING, roots, a, b, sa, sb, ro)
            return 1

        frame = a[ro[0]]
        print(f"PASS dso-share ({ARCH_NAME}): all {len(ro)} read-only page(s) of the"
              f" object at 0x{DSO_BASE:x} are one frame set in two processes"
              f" (slots {sa} and {sb}; first page frame 0x{frame:x}); {len(rw)} writable"
              " page(s) are private, as intended")
        return 0
    finally:
        qemu.kill()


if __name__ == "__main__":
    sys.exit(main())
