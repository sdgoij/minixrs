# File-backed mmap exec (`vfs_memmap`) — status

Status: **Stage 3 (exec rework) implemented and booting on all three arches**
(x86, RISC-V, AArch64 — shell prompt, `hello` with threadstd, `exit`);
Stages 1–2 done in the simplified form below (per-page FDIO; the file page
cache has since landed — see §3/§6); Stage 4 partially done. The RISC-V and AArch64 ports needed three
additional boot fixes (§5 Bugs 4–6) before they could run the new exec, and
the RISC-V S-mode fault forwarding is now in-commit and boot-green alongside
the x86/AArch64 kernel-mode resume work.
Related: the kernel-mode fault-in work (§5 Bug 2 and §6) and the lazy-mmap work
this builds on.

## 1. Goal

Replace the "read the whole ELF into an anonymous buffer, then copy it into
the new image" exec with the real MINIX design: **map the executable's
segments from the file directly into the exec'd process's address space** and
demand-page them from the file. Concretely this:

- **Removes the 16 MiB executable cap.** Done: VFS reads only the ELF headers
  (`EXEC_HDR_MAX`, 8 KiB); the kernel's exec path has no ELF bound (only the
  `ARG_MAX` frame bound). `EXEC_ELF_MAX`/`EXEC_ELF_BUF` are gone.
- **Removes the full-image read** and the O(size) pre-touch — only the ELF
  headers are read at exec time; the image is demand-paged from the file.
- **Removes the exec-time cross-address-space kernel copies** (MFS's grant
  write, the `SYS_EXEC_LOAD` ELF copy) — no whole-image kernel copy exists.

## 2. Reference design (MINIX 3.3.0)

The C flow (`.refs/minix-3.3.0/`) — what this port's design is modeled on:

1. VFS `pm_exec` (`.refs/.../servers/vfs/exec.c`) opens the binary, installs it
   as the process's `vmfd`, and sets `execi.args.memmap = vfs_memmap`.
2. `libexec_load_elf` parses the ELF and calls `vfs_memmap` once per `PT_LOAD`
   segment — `vfs_memmap` (exec.c:160) forwards to `minix_vfs_mmap`, which
   sends VM a **`VM_VFS_MMAP`** (not `VM_MMAP`, which is the user mmap call)
   carrying `fd` + `file_offset` + `len` + `vaddr` + `clearend` + a lone
   `MVM_WRITABLE` bit from `PROT_WRITE` (not a full prot field).
3. VM's `do_mmap` sees a file fd and sends VFS a **`VMVFSREQ_FDLOOKUP`**
   request (`servers/vm/mmap.c:266`). VFS replies with `(dev, ino, size)`;
   VM's `mmap_file` (mmap.c:85) creates a **file-backed region** (VR_ANON
   absent; per-region `{dev, ino, fd}` + offset) at the segment's vaddr.
4. On the first touch of a file-backed page, VM's
   `mappedfile_pagefault` (`servers/vm/mem_file.c`) finds the block in VM's
   block cache (`servers/vm/cache.c` — *not* `vm_cache.c`, which does not
   exist); on miss it sends **`VMVFSREQ_FDIO`** to VFS. VFS forwards the read
   to the FS (`REQ_PEEK`), and the **FS** fills VM's cache page
   (`vm_map_cacheblock`); VM then maps the cache page into the process.
5. The kernel never copies the image: `do_exec`/`exec_restart` only set up
   registers and the stack (which VFS built and VM mapped).

**Deviation from the reference:** this port's VM *does* cache file pages — the
cache landed after this section was written (`crates/servers/src/vm/cache.rs`;
see §3) — so read-only file pages are shared across processes. What remains
per-process is a **writable** `MAP_PRIVATE` file page: `start_file_page` refuses
to cache one (`cacheable` requires `!writable`), so each fault allocates,
FDIO-fills and maps its own frame. There is still no file-page COW, and no
filesystem calls the `VM_SETCACHEPAGE` sharing protocol yet (§6), so MFS's own
buffer cache does not hand pages to VM.

## 3. Current-state inventory (updated)

| Piece | Where | State |
|---|---|---|
| mmap message carries fd/offset | `minix-std/src/vmem.rs`; `do_mmap` → `do_mmap_file` for user mmaps, `do_vfs_mmap` for exec segments | **used** |
| `VR_*` region flags | `servers/src/vm/region.rs` — `VR_FILE` + `{dev,ino,fd,file_offset,file_size}`, `VR_EXEC`, `VR_WRITABLE/READABLE`, plus `VR_SHARED` and `VR_CACHE` | **implemented** |
| VM↔VFS request codes | `arch-common/src/com.rs` (`VMVFSREQ_FDLOOKUP/FDCLOSE/FDIO`, `VM_VFS_REPLY`, `VM_VFS_MMAP`) | constants exist, used |
| VFS `do_vm_call` (FDLOOKUP/FDCLOSE/FDIO) | `servers/src/vfs/call.rs` | **implemented** (dupvm, resolve vnode → dev/ino; FDIO does `req_read` into the faulting page; reply normalized to OK — byte count travels in the payload) |
| VM→VFS request plumbing | `vm/vfs_request.rs` — asynchronous (`asynsend3` with `AMF_NOREPLY`), several in flight keyed by request id, a fixed node pool; the faulting process stays `RTS_PAGEFAULT`-blocked, VM does not wait | **implemented** (2026-09: was a synchronous SENDREC, finding 58) |
| VM `do_vfs_mmap` | `servers/src/vm/mod.rs` — creates one lazy VR_FILE region per exec'd `PT_LOAD` (per-segment prot, in-file end, clears identity PTEs, sets one-shot pre-fault) | **implemented** |
| VM `do_vfs_reply` | `vm/vfs_request.rs::complete` — matches the reply's request id to the outstanding request and completes it (the `mmap` answers its caller itself; a page fault finishes its page and resolves the fault) | **implemented** |
| VM file page cache | `crates/servers/src/vm/cache.rs` (`cache_find`/`cache_insert`/`clear_bydev`, LRU, 4096 pages); used by `vm/mod.rs::start_file_page`/`finish_page` | **implemented** — read-only and `MAP_SHARED`-writable file pages are shared across processes. The `do_mapcache`/`do_setcache`/`do_clearcache` handlers are implemented too (not stubs), but no filesystem calls them yet |
| `handle_pagefault_for` | vm/mod.rs | **file branch implemented**: `advance_fault` → `start_file_page` (a `vm/cache.rs` hit maps a shared frame and skips the rest; on miss: alloc → zero → map writable → FDIO → `finish_file_page`: zero bss tail → downgrade to region perms), the fault resolved by the page that lands last |
| VFS `vfs_memmap` | `servers/src/vfs/mmap.rs` | **implemented** (sends `VM_VFS_MMAP` with fd+offset+len+vaddr+prot+in-file end) |
| VFS `map_vnode` | mmap.rs | **implemented** — `req_newnode` to the mapped FS (PFS), sets `v_mapfs_e`/`v_mapinode_nr`/`v_mapfs_count`; returns ENOSYS only when `find_vmnt` finds no mount (the C panics) |
| `VM_EXEC_NEWMEM` / `do_exec_newmem` | vm/mod.rs | **wired into the exec chain** (VFS calls it before mapping segments) |
| Exec image construction | `kernel/src/syscall.rs::exec_elf_for_target` | **shrunk**: no ELF parse, no segment copy. Builds the fresh root (`exec_create_root`), clears the code range, maps stack + brk, sets up frame/registers. Entry + code range come from VFS via `SYS_EXEC_LOAD` |
| Identity-map aliasing at exec'd VAs | kernel clears the code range in the fresh table; `do_vfs_mmap`/`do_mmap_file` clear identity PTEs over the region | **cleared** |
| Exec pre-fault (non-exec regions) | `Vmproc::prefault_exec` (a flag in `vm/proc.rs`) + `Fault::for_prefault` (`vm/mod.rs`) | **implemented** (see §5, Bug-2 workaround) |
| `file_size` = in-file end | `region.rs` + `start_file_page`/`finish_page` — bss pages zero-filled, last partial in-file page tail zeroed | **implemented** (see §5, Bug-3 fix) |
| fdref / region teardown | `vm/mod.rs::fdref_close_if_unused` (FDCLOSE when the last region on a `(dev,ino,fd)` dies); `do_munmap` → `free_user_range`; `vm_destroy` clears regions | **implemented** |
| fork of file-backed images | `vm/proc.rs::vm_clone` copies regions verbatim (fdref shared); the kernel's `vm_paging_fork` builds the child table sharing frames and clearing the child's write bit, and VM's `cow_setup_fork` (`vm/cow.rs`) registers the COW views | **implemented** |
| 16 MiB exec caps | VFS header-only read; kernel no ELF bound | **removed** |

## 4. Architectural decision: who builds the exec'd address space

**Option A (VM-centric, incremental) was adopted** — the kernel keeps
`SYS_EXEC_LOAD` for stack/frame/register setup while VM owns the image's
regions and mapping:

1. VFS `pm_exec` (`crates/servers/src/vfs/exec.rs`) reads only the ELF headers,
   parses `e_phnum`/`e_phdr`, and validates each `PT_LOAD` against the file
   size.
2. VFS sends `VM_EXEC_NEWMEM` — VM clears the old region list (closing file
   vmfds) and re-establishes the heap region.
3. VFS sends `VM_VFS_MMAP` once per `PT_LOAD` — VM creates a lazy VR_FILE
   region (per-segment `VR_WRITABLE`/`VR_EXEC`, `file_offset`, and the
   **in-file end** `p_offset + p_filesz`).
4. VFS sends `SYS_EXEC_LOAD` with entry + code range + the exec frame — the
   kernel builds the fresh page table from the boot identity map
   (`exec_create_root`), **clears the code range** so the lazy file regions
   fault on first touch, maps stack + brk, and programs the entry registers.
5. Demand-paging: on a file-region fault, `start_file_page` allocates a private
   page, zeroes it, maps it writable, sends `VMVFSREQ_FDIO` (VFS `req_read`s
   the file block into the page through the target's CR3), zeroes the bss tail
   past the in-file end, and downgrades to the region's permissions. (A
   `vm/cache.rs` hit short-circuits this — see §3.)

Two Stage-3 additions sit on top of this flow (see §5):

- **One-shot exec pre-fault** of non-executable file regions, and
- the **in-file end** bss semantics.

## 5. Bugs found and fixed while landing Stage 3 (all three arches)

These are the issues that kept the boot/exec from being green on each arch;
all are now fixed and boot-verified.

### Bug 2 — kernel-mode vircopy faults on lazy non-exec pages (pre-fault workaround)

After the exec chain started working, the shell hung at its prompt because
VFS's kernel-mode `sys_vircopy` of the process's user buffer (e.g. the
shell's `write(1, "# ", …)` where the prompt lives in lazy `.rodata`) faulted
in kernel mode (CPL=0). The x86 kernel-mode fault **resume** was not
implemented then (`save_fault_context` could not recover a ring-0 frame from
the IST1 #PF), so the fault livelocked into a #GP.

Fix (Stage-appropriate, no kernel-mode resume machinery): on the **first
file-region fault after exec**, VM pre-faults every **non-executable** file
region (rodata/data/bss) so VFS's kernel-mode copies of the image hit present
pages. Text (`VR_EXEC`) stays lazy — its faults are user-mode instruction
fetches, which have a working resume path. Implemented as a
`Vmproc::prefault_exec` flag (`vm/proc.rs`) set at exec and consumed by
`Fault::for_prefault` (`vm/mod.rs`, selecting `VR_FILE && !VR_EXEC` regions);
`start_file_page`/`finish_page` are the per-page mapper. The x86 kernel-mode fault
resume (conditional `swapgs` + `restore()` IRET by saved CS RPL) has since
landed with this commit, so the pre-fault is no longer
strictly required on x86 — it stays as a uniform, cheap workaround across
arches.

### Bug 3 — bss pages filled with ELF file garbage (in-file end semantics)

The fork+exec of external binaries (`hello`, `coreutils`) deadlocked: the
forked child blocked in `futex_wait` on `minix-rt`'s `BRK_LOCK` (a `.bss`
static) inside `execve`'s `sbrk`, because the lock read as a nonzero garbage
value.

Root cause: the region's `file_size` meant *whole file size*. A binary's bss
lives in a separate PT_LOAD (`filesz=0, memsz>0`), so its pages' file offsets
fall *inside* the ELF file's tail sections (`.symtab`/`.strtab`). VM's
`file_off < file_size` check treated them as in-file and FDIO-filled the "bss"
page with file metadata.

Fix: the region's `file_size` is now the **in-file end** for the region —
`p_offset + p_filesz` for exec segments (VFS parses and sends `p_filesz`),
`min(mapped-end, EOF)` for user mmap. Pages at or past it are zero-filled;
the last partial in-file page's tail (past the segment's `filesz`) is zeroed
after the FDIO read.

### Bug 4 — RISC-V: exec'd text pages lack the SV39 execute (X) bit

RISC-V hung at `init: starting shell...` — the shell's exec never completed
and no prompt appeared. QMP walks of the exec'd page table showed the text
PTE as `0x13` (V|R|U): the page was *present* but not executable, so every
instruction fetch faulted, and because the page was present the demand-paging
present-skip in `start_file_page` returned without remapping — an instruction-
fetch livelock (zero page faults in the QEMU trace, init oscillating between
`RTS_PAGEFAULT` and runnable with `sepc` pinned at the entry point).

Two compounding causes:

1. **No `MAP_EXEC` flag existed** in the kernel's flag vocabulary — `map_page`
   flags carried `MAP_USER` (+`MAP_WRITE`) and RISC-V enforces X (x86 has no
   execute bit; absence of NX means executable, so this was invisible there).
2. **VFS never sent `PROT_EXEC`**: `pm_exec` built `prot` from `p_flags` as
   read-*or*-write, so VM's `do_vfs_mmap` never marked the text region
   `VR_EXEC`, and `finish_page` downgraded it to read-only.

Fix: add `MAP_EXEC` to all three HALs (`PTE_X` on RISC-V, `0` on
x86/aarch64 — absence of NX/UXN means executable there), honor `VR_EXEC` in
`finish_page`'s downgrade, the anon demand path, and `do_mmap`, and have
`pm_exec` convert ELF `p_flags` (PF_R/W/X) to `PROT_READ/WRITE/EXEC`. Also
fixed `pte_leaf_flags()` to set `PTE_R`: SV39 decodes a leaf with R=W=X=0 as
a table pointer, so a "read-only" user page faulted on every access.

### Bug 5 — AArch64: brk heap collides with the kernel's EL1-only identity block

AArch64 hung inside `hello`'s `threadstd` (after `alloc: churn + realloc +
align ok`), livelocked on an **EL1 prefetch abort** at the kernel's own first
page (ELR=FAR=0x400000e8, `aarch64_pf_handler`). The root cause was the
userland heap, not the EL1 fault path: the brk heap (0x3FE00000, 2 MiB below
the kernel's EL1-only identity block at 0x40000000) grew past the block —
`hello`'s four 256 KiB thread stacks (`sbrk`) plus the allocator churn were
enough — and `map_page` split the 1 GiB kernel block, installing a **user
page at 0x40000000** (the kernel's code page). The next syscall from that
thread branched into kernel page 0, took an EL1 prefetch abort, and the
handler eret'd back to the faulting instruction — livelock.

Fix: relocate the aarch64 user heap below the anonymous-mmap base. New
per-arch HAL functions `user_heap_base()`/`user_heap_limit()`:

- aarch64: `0x2000_0000` / `0x3000_0000` (256 MiB of heap, clear of the
  kernel block at 0x40000000 and of mmap regions at 0x30000000+);
- x86/riscv: `0x3FE00000` / `0x1_0000_0000` (unchanged).

Threaded through the kernel exec brk pre-mapping (`exec_elf_for_target`),
kernel-boot's aarch64 boot-proc brk mapping, VM (`vm_init_boot`,
`do_exec_newmem`, and `do_brk` now caps growth at the heap limit), and
minix-rt (`HEAP_BASE`/`HEAP_LIMIT` per-arch).

### Bug 6 — RISC-V: S-mode trap entry corrupted the interrupted t4 register

Re-applying the S-mode fault forwarding on the boot-green tree hung `hello`
at `pid=12`. The QEMU `-d int` trace showed an ecall livelock at `0x1005ac6`
and a store page fault to `tval=0x80276dc8` — the kernel static
`trap_scratch`.

Root cause: a register-preservation bug in the fault-in entry. It ran
`la t4, trap_scratch; sd t0, 0(t4); sd t4, 8(t4)` — storing the
already-clobbered t4 (the *address* of `trap_scratch`) into the frame's t4
slot. Every trap frame therefore handed the user a kernel pointer in t4;
`hello`'s memcpy dereferenced it and wrote into the kernel static.

Fix: rewrite the entry so nothing user-visible is clobbered before the frame
save. `csrrw t0, sscratch, t0` parks the interrupted t0 in sscratch (restored
from the frame); `sstatus.SPP` picks the stack — U-mode traps run on the
fixed `__boot_stack_top` kernel stack, S-mode faults keep the interrupted
kernel SP with the 296-byte frame below it and the SP spilled below the frame
(the mid-syscall stack and its U-mode trap frame stay intact for the eventual
syscall-exit `sret`). The `trap_scratch` static is gone. `trap.rs` forwards
faults taken in either mode, synthesizing kernel error codes without the user
bit but keeping the write bit (store 0x03, load 0x01, instr 0x10) so VM's
demand-paging maps the page writable and the retried store succeeds; the
post-syscall hook switch resumes the blocked process via `sret` in the same
mode it faulted in.

## 6. Open questions / deferred items

- **File page cache** (`crates/servers/src/vm/cache.rs`): **implemented**. Read-only and
  `MAP_SHARED`-writable file pages are cached by `(dev, offset)` with a `PhysBlock`
  reference and shared across processes; a writable `MAP_PRIVATE` page keeps the private
  allocate + FDIO path, which is correct (a shared frame must not serve a private
  writable mapping). The v2 item that *is* still open is **FS-side** cache sharing: VM's
  `VM_MAPCACHEPAGE`/`VM_SETCACHEPAGE`/`VM_CLEARCACHE` handlers exist and are registered,
  but no filesystem in the port sends them, so MFS's own buffer cache does not donate
  pages to VM (and read-ahead stays absent — see the I/O-amplification item below).
- **Kernel-mode fault resume**: the exec pre-fault only covers
  exec'd images' non-exec regions. Any other kernel-mode copy into a lazy
  user page (future lazy stack/heap, general `vircopy`) still requires the
  real kernel-mode resume machinery. Status: all three arches are in-commit
  and boot-verified — **x86** (conditional `swapgs` + kernel-mode IRET),
  **AArch64** (EL1 rework: full frame save + ESR-synthesized error codes),
  and **RISC-V** (S-mode forwarding; the "does the trap-asm frame save capture
  the S-mode SP?" open question turned out to be an asm register-save bug —
  §5 Bug 6 — now fixed). The lazy exec only takes
  user-mode faults in practice (text fetch + pre-faulted rodata/data), which
  have a working resume path on every arch. The **pre-fault workaround
  stays** because of the Phase 6 attribution gap, not a resume bug: with it
  disabled, the RISC-V S-mode forwarding fires (~122K S-mode faults; the
  first load fault resolves) but the kernel's `virtual_copy` switches the
  address space mid-copy and attributes the fault to the *copier* (e.g.
  VFS), not the page-table owner — VM resolves the copier's regions, finds
  no match, and SIGSEGVs the process.
- **clearend** semantics are subsumed by the in-file end handling (§5 Bug 3),
  including the mid-page tail zeroing.
- **COW for shared file pages**: `MAP_SHARED` file regions are implemented and the
  region flag is set (`vm/mod.rs`); `vm/cow.rs` treats a `VR_SHARED` page as an
  exception at fork — the alias stays live, so parent and child keep writing the
  same frame. There *is* a consumer now: `mmapfd shared`
  (`userland/src/lib.rs::mmapfd_shared`). This is no longer "deferred, no userland
  consumer yet".
- **Userland `mmap(fd)`**: exercised by `/bin/mmapfd` (§7) and `mmapfd shared`;
  exec remains the production user. The `do_fstat` gap recorded here does **not**
  hold in the current code: `do_fstat` → `req_stat` uses a write grant
  (`vfs/call.rs`, `vfs/request.rs`) and MFS `fs_stat` copies the `Stat` back
  (`fs/src/mfs/stadir.rs::stat_inode` → `safecopy_to`). mmapfd still avoids `fstat`
  (it uses `lseek(SEEK_END)`), but that is a choice, not evidence of the bug — if a
  stat-using tool misbehaves, re-check this path at runtime.
- **Repeated-exec degradation (leak)**: the 20× exec loop passes on all
  arches at all RAM sizes, but a 100× loop degrades and hangs — `hello`
  stops completing at ≈44 execs (`-m 256M`) / ≈63 (`-m 4G`) on x86
  (memory-dependent). Prime suspect: the 4 kernel thread stacks `hello`
  spawns not being reclaimed at process exit (176–252 stacks × ~1 MiB
  before the hang). Next probe: dump kernel memory at the hang (free-pool
  size, thread-stack usage) to confirm, then fix the teardown. A
  per-exec region/page count check (qmp_state.py-style) should be
  automated once the leak is fixed.
- **MFS read-path I/O amplification**: large sequential reads show ~3-6×
  more virtio-blk I/Os than the file's block count (cat ≈2.8×, mmapfd
  ≈5.6×, the 33 MiB `/bin/bign` pre-fault ≈5.8× — 40-48K reads for 8467
  blocks). Not exec-specific and not resolved by two candidate cache
  fixes (make PREFETCH read; move `ONE_SHOT` off the `FULL_DATA_BLOCK`
  constant value — both tried and reverted, §7). Open: profile MFS's
  cache/read-ahead against the virtio request stream.
- **AArch64 address-range ceiling** (was a caveat): a proper user-VA ceiling now
  exists — `MAX_USER_ADDRESS = kern_vaddr() = 0x4000_0000`, with a compile-time pin
  (`arch-aarch64/src/vmparam.rs`), so a kernel-range VA is no longer "user-range" to
  the address-based gate. The heap fix (Bug 5) removed the one real trigger. This is
  closed, not an open follow-up.
- **Attribution**: exec no longer does cross-address-space kernel copies, so the
  exec-time attribution exposure is gone. General `vircopy` attribution stays a
  separate open item (`KNOWN_ISSUES.md` #5).

## 7. Verification (current status)

Done on all three arches (x86, RISC-V, AArch64):

- Boot to shell prompt (`# `); `hello` (alloc stress + 4 threads joined via
  threadstd); `exit`.
- RISC-V required the §5 Bug 4 (SV39 X bit) and Bug 6 (trap-entry t4
  corruption) fixes; AArch64 required Bug 5 (heap/kernel-block collision).
- The RISC-V S-mode fault forwarding is boot-verified with the pre-fault
  active; with the pre-fault disabled it resolves the first load fault and
  then hits the §6 attribution gap (expected).
- `cargo clippy -- -D warnings` clean; `cargo test -p servers -p kernel
  -p minix-rt` host tests green.

Exec loop (`tools/exec_loop_run.py`, 20× `hello`, monotonic PIDs) at
`-m 256M`/`1G`/`4G` — **PASS on all three arches** (9 boots).

Large-binary test (`tools/std-big.rs` → `/bin/big`, 34.6 MiB ELF with a 33
MiB `.rodata` PT_LOAD) at `-m 256M`/`1G`/`4G` — **PASS on all three arches**
(9 boots): exec completes past the old 16 MiB cap, all 8448 pages
demand-page in from the file, and the checksum matches the file contents
(8448 × 0xAB). The build hooks that make this possible: `MINIXFS_BLOCKS`
(image size, default 4096 blocks / 16 MiB — `boot-image/src/minixfs.rs`) and `MINIXFS_EXTRA`
("dest=path;…" binaries injected into the disk image only — the initramfs
stays small so the kernel image fits the 256M budget). MSYS converts
POSIX-style values (`/bin/big=…`) in env vars on Windows, so the invocation
must set `MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA`.

`mmap(fd)` consumer — **new `/bin/mmapfd` userland binary** (open + mmap
MAP_PRIVATE, verify the mapping matches the file page-by-page including the
partial last page, verify a writable private mapping does not modify the
file). **PASS on all three arches** (x86 45 pages, riscv 61, aarch64 66).
This is the first exercise of the userland file-mmap path (`do_mmap_file` +
FDLOOKUP + file regions). Two gaps found while landing it: VFS `do_fstat`
never copies the `Stat` back to the caller (mmapfd uses `lseek(SEEK_END)`
instead), and `MINIXFS_EXTRA` entries are routed by string match (to `/bin`, `/sbin`
or `/etc`, else the root; `build.rs` now rejects other destinations) because
MSYS mangles POSIX dest paths (the `Path::parent()` match put them
in `/`).

Boot fixes required by the enlarged images (embedded initramfs + minixfs
now reach ~100 MiB in the kernel image):

- `kernel-boot` aarch64/riscv64 hardcoded the allocator start 16/14 MiB
  above the load base; the embedded root image far exceeds that, so boot
  processes were loaded over the kernel's own embedded data (aarch64
  faulted with the EL1 unknown-EC `X`). Both now use the linker's
  `__kernel_end` (as x86 already did), keeping the allocator clear of the
  whole image.
- `boot-image::minixfs` only wrote direct + single-indirect zones (≈4 MiB
  max file); a ≥32 MiB file needs the double-indirect chain, which MFS's
  `read_map` already resolves but the builder never wrote. Added it (zone
  [8], with the same layout read_map expects).
- With the whole image embedded, `-m 256M` left too little RAM for repeated
  `hello` (thread spawn EAGAIN at exec ~10); excluding `MINIXFS_EXTRA` from
  the initramfs and sizing the minixfs to 48 MiB fixed it.

Read-traffic probe (`tools/read_trace_probe.py`, counts virtio-blk
completions via QEMU `-trace`):

- VFS's exec read is header-only (code-verified: `hdr_len =
  min(file_size, EXEC_HDR_MAX=8192)`, a single `req_read`).
- The full-image reads an exec triggers come from **VM's exec pre-fault**
  (§5 Bug 2): the first file-region fault after exec pre-faults every
  non-executable region, so `/bin/bign`'s 33 MiB `.rodata` is read eagerly
  (≈8467 block reads) even though its main never touches it. Text stays
  lazy. Removing this workaround requires the fault-attribution fix (see
  `KNOWN_ISSUES.md` #5), not a VFS exec-path change; the port chose the
  C-faithful alternative first — pre-check both sides of every kernel copy and
  return `EFAULT` — under which an eager pre-touch is the model, not a
  workaround.
- Large sequential reads also show a **pre-existing ~3-6× I/O
  amplification** in the MFS/virtio read path (cat ≈2.8×, mmapfd ≈5.6×,
  bign ≈5.8× the file's block count). Not exec-specific; two candidate
  fixes (make PREFETCH read; move `ONE_SHOT` off the `FULL_DATA_BLOCK`
  constant value) were tried and reverted — neither resolved it. Open
  follow-up: profile MFS's cache/read-ahead against the virtio request
  stream.

Region-stability stress — **resolved**: the 100× loop used to degrade and
hang (`hello` stopped completing at ≈44 execs at `-m 256M`). Root-caused
and fixed as a cascade of per-exec leaks:

- **VM fdref self-reference** — `fdref_close_regions` (exec/exit) closed
  vmfds only when *no process* referenced the (dev, ino, fd), but the
  dying process's own still-present regions always matched, so VFS's
  `fp_filp[64]` never got its FDCLOSE and filled 1 fd/exec (wedge at
  ~OPEN_MAX execs). The scan now excludes the dying endpoint; VFS fd count
  is flat (1-2) across the loop. (`fdref_close_if_unused` gains an
  `exclude_ep`; `vmfd_is_referenced` extracted; host test
  `test_fdref_scan_excludes_the_dying_process`.)
- **VM self-map VA march** — `vm_find_hole` bumped a monotonic counter on
  every temporary map (cow walks, page-table walks, zero-fill), so VM's
  own address space consumed a fresh kernel PT page per 512 maps (~1.2
  pages/exec; the kernel's `unmap_page` keeps intermediate tables). The
  unmapped VAs are now returned to a small LIFO and reused
  (`VM_MAP_VA_FREELIST`), bounding VM's mapping region to the peak
  concurrent mappings. Host test `test_vm_find_hole_reuses_released_va`.

Verification (`tools/exec_loop_mem.py 400 256M 25` and repeated 200×
runs): free pages are **flat** — 48713 at boot and after 400 execs, leak
0.0 KiB/exec. The driver was also hardened against its own false-wedge
(stale last-N-bytes pid=/threadstd matches let memstat land mid-exec and
report an 8s timeout as a hang): it now matches markers positionally
after each send and syncs on the shell's prompt. The bitmap-level check
is automated in `tools/alloc_probe.py` (dumps the kernel allocator
bitmap via QMP and diffs intervals).

AArch64 exec-2 hang — **resolved**: the second `hello` exec always hung
with an unknown-EL1 `X` (EC 0x00 UDF) while virtio_blk was scheduled; its
text frame 0x41bb2000 was zero-filled and marked free in the bitmap. Root
cause: `pte_user_owned` reads the low-GB alias window from
`crate::alloc::base()`/`usable_size()`, but VM's copy of the arch
allocator is never initialized (`init_phys_alloc` is a no-op on aarch64),
so in VM's binary both read ZEROS — `win_size == 0` disabled the alias
check and every alias leaf in a split block was freed as process-owned.
Once hello's heap grew into block 257 (VA 0x20200000, alias frames
base+14..16 MiB), teardown freed virtio_blk's and virtio_net's live text;
the next exec re-allocated those frames as zeroed table pages and
virtio_blk #UD'd. Fix: both kernel and VM now use a cached window — the
kernel sets it after `init_allocator` (`set_alias_window`), VM queries it
via kernel call 62 / `VM_PAGING_MEMINFO` (11) in `vm_main`.
Verification: `exec_loop_mem.py` at `-m 256M`/`1G`/`4G` — all execs
complete and the leak is **0** on all three arches. The initial aarch64
run showed a 3 pages/exec residual; it was traced to the aarch64 fork
deep-copying the shared low-GB alias leaves (~1400 pages/exec of
boot-server code/data into frames freed again at exec) plus the dead
`cow_setup_fork` walk (aarch64 forks are deep copies, so the COW setup
registers nothing). The fork now shares alias leaves verbatim; then, with
the COW fork landed (next paragraph), the COW setup is active on aarch64
too; `just test-boot-aarch64` green.

AArch64 COW fork — **implemented**, replacing aarch64's deep-copy shortcut:
`vm_paging_fork` now shares frames and marks only
the child's view read-only (AP = `PTE_AP_RO`, the parent's PTE untouched);
alias leaves stay shared verbatim; VM's `cow_setup_fork` + the COW
message-buffer prefault are active on aarch64. Because AP[2:1] is a 2-bit
field (not a single RW bit, `PG_RW == 0` on aarch64), writability now goes
through HAL helpers (`pte_is_writable`/`pte_set_writable`/`pte_is_user`),
and teardown's `pte_user_owned` recognizes AP=11 leaves so COW frames are
unref'd at exit instead of pinned. New userland verification binary
`/bin/forktest` (fork + write isolation: child writes 0xBB to a shared
`.data` page, parent's view stays 0xAA, parent writes 0xCC) — **PASS on
all three arches**; the exec loop stays flat (leak 0) at 256M/1G/4G.

Two cross-arch bugs found while landing it (both fixed):

- `do_vfs_mmap` removed a whole overlapping region when a later PT_LOAD
segment shared its rounded-up tail page (a `.data` segment whose memsz
rounds to a second page that the `.bss` start page also claims) — the
`.data` first page ended up with no region and every fault on it
looped. It now trims the old region to its non-overlapping part.
(`crates/servers/src/vm/mod.rs::do_vfs_mmap`)
- VM's `sys_kill` called `send_sig` (records the bit in `s_sig_pending`,
notifies SYSTEM — never PM), so a fault with no matching region left
the process alive and re-faulting forever (VM spun in a memreq_get /
handle_pagefault_for livelock). It now goes through **`SYS_KILL`** (`minix_rt::kernel_call(6, …)`)
so the real kernel `Proc` is signalled — a direct `cause_sig` from VM does
not work, because VM links its own copy of the kernel crate whose tables
nothing reads. An unhandleable fault now kills the process.
(`crates/servers/src/vm/mod.rs::sys_kill`)

Remaining:

- FS-side cache sharing (v2): VM's file page cache, its LRU eviction and the
  `do_mapcache`/`do_setcache`/`do_clearcache` handlers have landed (§3, §6), so
  read-only and `MAP_SHARED` file pages are shared across processes; what remains is
  for a filesystem (MFS's own buffer cache) to donate pages through
  `VM_SETCACHEPAGE`, which none does yet.
- MFS read-path I/O amplification (above); the exec-leak is resolved on
  all three arches (x86/riscv 0 KiB/exec; aarch64 reached 0 after the
  fork stopped deep-copying alias leaves — see the aarch64 paragraph
  above).
- `mmap(fd)` MAP_SHARED semantics (v2, no consumer yet).
