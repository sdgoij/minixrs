# Dynamic linking for minixrs — implementation proposal

Status: **proposal — design settled, Phases 0–3 built, Phase 4 decided (dropped), Phase 5
measured and passing on x86_64 and riscv64, Phase 7 done** (on
`feature/ldso`). Every claim
below about the port was read out of the tree at the time of writing; every claim about
MINIX 3.3.0 comes from `.refs/minix-3.3.0/` and is cited by file. Sections marked *as
built* record where the implementation revised the draft.

Related: [`FILEMMAP.md`](FILEMMAP.md) (the exec/mmap substrate this builds on),
[`PORTING_PLAN.md`](PORTING_PLAN.md) (phase tracker), [`C_BUILD.md`](C_BUILD.md) (the
toolchain this has to extend), `.agents/skills/minix-kernel-boundary` and
`silent-failure-traps`.

## TL;DR

- The port is **all-static, `ET_EXEC`-only, non-PIE at `0x01000000`**, but it already
  has the two things a loader is built on: **file-backed demand-paged exec**
  (`FILEMMAP.md`) and **userland `mmap` with `MAP_FIXED` and `MAP_SHARED`**
  (`vm/mod.rs::do_mmap`, `finish_mmap_file`). So this is an *addition* to a working
  substrate, not a replacement of it.
- **The payoff is nearly free — now measured, and the one hole closed.** VM's file page cache
  shares a read-only DSO page across processes, as the draft claimed, and Phase 5 also made
  the *simultaneous* case work: two lives that reach a page together become one fill rather
  than two, so every read-only page of `libc.so` is one frame set in two processes started as
  a pipeline — 13 of 13 on x86_64 (`just probe-dso-share-x86`) and 11 of 11 on riscv64
  (`just probe-dso-share-riscv64`) — with each mapping's writable `.data`/`.got` private. One
  soundness fix came with the measurement: the cache key did not include the inode. Phase 5 (§7).
- The kernel ELF loader needs almost no change: the **exec path never checks `e_type`**
  (`servers/src/vfs/exec.rs::pm_exec` reads `PT_LOAD`s directly), and DSOs are mapped by
  *userland* `mmap`, not by `parse_elf_header`. The one kernel-side edit is a register
  convention so the loader learns where the main program is (`main_hdr`, §6.1–6.2).
- Recommended shape: a **new Rust `ldso` crate** installed as `/libexec/ld.so`, an
  **opt-in** dynamic build (default stays static, exactly as MINIX does), and a
  **classic non-PIE first** progression so each phase adds exactly one mechanism.
- **wasm32 is out of scope** — exec there is host module instantiation and there are no
  faults to fault on (`ARCH_WASM32.md`). This is a three-arch project.
- **The toolchain fork carries the PIC targets after all — and the static path is untouched.**
  Phases 0–3 first ran on a JSON target plus `-Z build-std`, which needed a nightly; the targets
  are now the fork's own `*-minix-elf` triples, built by the same stage1 compiler as the rest of
  the port, with `no-std` sysroots holding the PIC `core`/`alloc` a shared object links (§6.5).
  The *executable* specs are byte-for-byte what they were, so static stays exactly what it was
  (D2), and no artifact in the tree needs a nightly.
- **Phase 3 is a dynamic C library.** `minix-libc` builds as `libc.so`, and a C program
  linked against it runs — which needed the loader to place thread-local storage, since
  the library has `#[thread_local]` statics of its own (§6.6, D8).

## 1. Goal

Make it possible to build and run programs that link against a shared object at load
time, and to share that object's text between processes, on the three hardware arches —
without changing the behaviour of any existing static binary.

Concretely, "done" for v1 means:

- `/bin/dynhello` prints a string it does not contain, which comes from `/lib/libdyn.so`
  (proved by the string living only in the `.so` in the image).
- A C program linked against `/lib/libc.so` runs, and what it prints comes from that
  object — including an `errno` read out of thread-local storage the loader placed.
- Two processes running `dynhello` share the `.so`'s text pages (measured, not assumed).
- A `fork` after load works (the child keeps the mapping), and re-`exec` works.
- Every existing static binary and every existing gate is unchanged.

### Non-goals for v1

- **Symbol versioning** — there is no compatibility surface to version.
- **Lazy binding** — eager binding is simpler, has no PLT trampoline and no
  `_dl_runtime_resolve`, and is the right default for an OS with no ABI promises yet.
- **ASLR** — the port has none today; the loader will use a deterministic base.
- **`ldconfig` / `/etc/ld.so.cache`** — a fixed search path plus `LD_LIBRARY_PATH`.
- **wasm32** — see §1 of `ARCH_WASM32.md`: no page faults, exec is instantiation.

## 2. Why this is opt-in, and what it buys

### 2.1 MINIX itself defaults to static

The reference is explicit (`share/mk/bsd.own.mk`, `defined(__MINIX)`):

```
LDSTATIC?=	-static
MKDYNAMICROOT?=	no
```

and the ramdisk only carries the loader when `LDSTATIC == "-dynamic"`
(`minix/drivers/storage/ramdisk/Makefile`, `proto.common.dynamic`). The linker exists
(`libexec/ld.elf_so/`), `dlopen` exists (`lib/libc/dlfcn/dlfcn_elf.c`), the kernel and
PM support `PT_INTERP` (`minix/servers/vfs/exec.c`, `minix/lib/libexec/exec_elf.c`) —
but a default MINIX build does not use any of it. The port should copy that posture:
add the machinery, change no default.

### 2.2 What it actually buys the port

- **Shared text between processes.** File-backed read-only pages are shared by VM's file
  page cache (`crates/servers/src/vm/cache.rs`, consulted by `start_file_page` and
  populated by `finish_page`), which Phase 5 measured for a DSO, on x86_64 and riscv64: all
  of `libc.so`'s read-only pages are one frame set in two processes. The DSO case is
  therefore *different* binaries sharing one object's text — what a C library buys, and
  what Phase 3 delivered; two processes of the *same* binary get it free. What stays
  private per process either way is the writable, relocated part (`.data`/`.got`) and
  every monomorphised instantiation (`tools/rc-cost.py`), and that is what bounds the
  saving. Phase 5 measured the sharing and closed the simultaneous-start case. See §2.3 and
  Phase 4's decision.
- **Smaller C binaries and a smaller image** — for the programs that link `minix-libc`, which
  is where the copies were: `bash` and `ctest` carry it today and `libc.so` is not carried at
  all. Phase 3 is the size of it; a Rust binary's library code is largely inlined, so this does
  not extend to the userland (Phase 4).
- **A home for a driver/plugin ABI later** (Phase 6), which is what `dlopen` is for.
- **Fidelity**: it is the one MINIX subsystem the port has no representation of at all.

### 2.3 What it does not buy

Startup is slower (parse + relocate), it is a new failure surface on the exec path, and
it will not shrink the *statically linked* binaries that exist today unless they are
rebuilt to use it. No other subsystem waits on it. The plan is written so the early
phases are individually abandonable.

## 3. Reference design (MINIX 3.3.0)

What the C system does, in the order it happens — this is the contract the port mirrors:

1. **VFS notices the interpreter.** `minix/lib/libexec/exec_elf.c::elf_has_interpreter()`
   reads `PT_INTERP`. `minix/servers/vfs/exec.c::pm_exec` then marks the exec
   `execi.is_dyn = 1`, opens an fd for the *main* program (`execi.elf_main_fd`,
   exposed as `AT_EXECFD`), and **switches the executable to the interpreter**
   (`fullpath = elf_interpreter`, `Get_read_vp` again).
2. **The loader is placed away from NULL** (exec.c, the comment "ld.so is linked at 0,
   but it can relocate itself"):
   `execi.args.load_offset = stack_high - stack_size - 0xa00000` — 10 MB below the stack.
3. **Aux vectors are patched onto the stack.** `exec.c::stack_prepare_elf` walks the
   frame libc built (`minix/lib/libc/sys/stack_utils.c::minix_stack_fill`) and fills
   `PMEF_AUXVECTORS` (20) entries: `AT_BASE` (loader base), `AT_ENTRY` (main program
   entry), `AT_EXECFD`, `AT_EUID`, `AT_EGID`, `AT_PAGESZ`, then
   `AT_SUN_EXECNAME` + `AT_NULL`.
4. **The kernel/handler starts the loader**, not the program. `stack_utils.c` documents
   the register contract: `*fct` (the loader's entry), `*ObjEntry` (the main program's
   entry) and `*ps_string` are passed to `_rtld_start`.
5. **`ld.elf_so` does the rest** (`libexec/ld.elf_so/rtld.c`): parse `.dynamic`, map each
   `DT_NEEDED` from the search path, load `PT_LOAD`s, process `RELATIVE`/`GLOB_DAT`/
   `JUMP_SLOT`/`COPY`/TLS relocations (`reloc.c`, `arch/*/mdreloc.c`), resolve symbols
   (`symbol.c`, `search.c`), set up TLS (`tls.c`, `README.TLS`), run initialisers, and
   jump to `AT_ENTRY`.

Two details worth carrying over verbatim: the **`_rtld_start(fct, ObjEntry, ps_string)`
register contract** (it avoids needing a full auxv on the first cut, §6.2), and the
**fixed loader reservation below the stack** (it avoids relocating the relocator, §5.3).

## 4. Current state: what the port already has

| Piece | Where | State |
|---|---|---|
| ELF parse for exec | `servers/src/vfs/exec.rs::pm_exec` | reads `e_phoff`/`e_phnum`/`e_entry` + `PT_LOAD`s **only**; **no `e_type` check**, so a non-`ET_EXEC` main program is not rejected here |
| Kernel ELF loader | `kernel/src/elf.rs::load_elf`, `parse_elf_header` | `PT_LOAD` only; **hard-rejects anything but `ET_EXEC`** (`ElfError::NotExecutable`) — this is the *initramfs/boot* path, not the exec path |
| `PT_INTERP` / `PT_DYNAMIC` | `kernel/src/elf.rs` | constants declared, **never read** |
| Exec pipeline | `vfs/exec.rs` → `VM_EXEC_NEWMEM` → `vfs/mmap.rs::vfs_memmap` (`VM_VFS_MMAP`) → `SYS_EXEC_LOAD` | works, one image, one `vmfd` kept in **VFS's** fproc at `VM_PROC_NR` (never installed in the target) |
| Demand paging of segments | `vm/mod.rs::do_vfs_mmap`, `start_file_page`/`finish_page` | works; per-`PT_LOAD` lazy `VR_FILE` region, FDIO on fault (a `vm/cache.rs` hit first) |
| File page cache (sharing) | `vm/cache.rs` (`cache_find`/`cache_insert`/`clear_bydev`, LRU, 4096 pages); used by `vm/mod.rs::start_file_page`/`finish_page` | **implemented** — shares read-only and `MAP_SHARED`-writable file pages across processes; `VM_MAPCACHEPAGE`/`VM_SETCACHEPAGE`/`VM_CLEARCACHE` handlers are implemented too (the `FILEMMAP.md §3/§6` "stubs" rows are stale) |
| Userland `mmap` | `minix-std/src/vmem.rs` → `vm/mod.rs::do_mmap` | works; **`MAP_FIXED` honoured** (`do_mmap`, `finish_mmap_file`, `do_vfs_mmap`), `MAP_SHARED` sets `VR_SHARED` |
| Identity-PTE clearing | `vm/mod.rs::finish_mmap_file` (`vm_clear_range`) and `do_vfs_mmap` | **already clears identity PTEs over a mapped range** — so a userland `mmap(MAP_FIXED)` at `0x01000000` will fault in correctly |
| Region flags | `vm/region.rs` | `VR_FILE`, `VR_EXEC`, `VR_WRITE`, **`VR_SHARED`** (documented as the exception to the private-page model) |
| VM↔VFS protocol | `arch-common/src/com.rs` (`VMVFSREQ_FDLOOKUP/FDCLOSE/FDIO`), `vm/vfs_request.rs` | async, several in flight, keyed by request id |
| Stack/frame construction | `kernel/src/elf.rs::setup_user_stack_full`, called from `kernel/src/syscall.rs::exec_elf_for_target` | builds argv/envp+strings at **physical** pages and relocates the pointers; **no auxv, no `ps_strings`** |
| Entry/register setup | `hal::exec_init_regs(frame, entry, sp, argc, argv)` | per-arch; no third "loader" argument |
| Runtime entry | `minix-rt/src/lib.rs::_start`, `std/src/sys/pal/minix/mod.rs::_start`→`minix_start(argc, argv, envp)` | naked asm per arch; std PAL does `init_tls()` + `env::init(envp)` |
| TLS | `minix-user.ld` `.tdata`/`.tbss`, `__tls_start`/`__tdata_end`/`__tls_end`; PAL `init_tls` | **single module**: one block per thread copied from the image, no module id |
| Target spec | `rust/compiler/.../spec/base/minix.rs` | `dynamic_linking: false`, `relocation_model: Static`, `crt_static_default: true`, `pre_link_args = ["--image-base=0x1000000"]` |
| Userland link script | `tools/minix-user.ld` | fixed `BASE_ADDRESS = 0x01000000`; `.got`/`.got.plt` folded into `.data`; TLS sections defined |
| Linker | `tools/lld.py` (rust-lld), `CARGO_TARGET_*_LINKER` in the `Justfile` | GNU-flavoured LLD, no `-shared`/`-pie` use yet |
| Images | `boot-image/src/manifest.rs` (`BOOT_BINS`, `WASM_MODULES`), `crates/kernel/build.rs`, `MINIXFS_EXTRA`, `target/mkboot`, `target/mkfs` | binaries by cargo bin name; **no entry for a `.so` or a `/libexec` loader** |
| C build | `tools/build-c-hello.py` (clang + `tools/crt0-<arch>.S` + link the `minix-libc` rlib with the fork rustc), `tools/cc-minix.py` | all static; `crt0` calls `_start` |

**The load-bearing conclusion:** the exec substrate (file regions, `MAP_FIXED`, identity
clearing, the async FDIO path) is already the shape a loader needs. The missing pieces
are a loader binary, an interpreter branch in `pm_exec`, an entry-point register, image
manifest entries, and a toolchain that can emit a `.so`.

## 5. Architectural decisions

Each decision gives the options, the recommendation, and the cost of being wrong.

### D1 — Scope: the three hardware arches; wasm32 excluded

wasm has no page faults and `exec` is a host `instantiate`, so there is nothing for an
`ld.so` to do (`vfs/exec.rs` wasm arm, `ARCH_WASM32.md`). Gate the whole feature on
`not(target_arch = "wasm32")`, exactly as the existing ELF arm is gated.

### D2 — Default stays static; dynamic is opt-in

Mirror MINIX: no default changes. Add a `just` recipe and a build flag, keep the static
path bit-identical, and make every gate run both.

**As amended when the artifacts were shipped.** An image now *carries* the loader
(`/libexec/ld.so`), the shared C library (`/lib/libc.so`) and one dynamically linked C
program (`/bin/dynclib`) — all three in
`crates/boot-image/src/manifest.rs`'s `BOOT_BINS`, so every recipe that assembles an image
builds and embeds them, and the boot test asserts they are there. What that does not change
is the decision itself: every program the boot path runs is still static and non-PIE, VFS's
`PT_INTERP` branch is reached only by a binary that carries one, and linking a program
dynamically remains opt-in (`-Bdynamic` plus `--dynamic-linker`, per §6.5). An image with
the three artifacts boots exactly as one without them, which is what "nothing in the boot
path changes" has to mean for the claim to be worth anything. The cost is the three files
in both images (≈200 KiB per image, and the loader is 33 KiB of it).

### D3 — A new Rust loader crate, not a port of `ld.elf_so`

The project is from-scratch Rust with no C runtime to host NetBSD's `ld.elf_so`
(`xmalloc.c`, `xprintf.c`, `rtldenv.h` would all need a C libc). Write
`crates/ldso` (bin `ldso`, installed as `/libexec/ld.so`). Structure it so the ELF and
relocation logic is **`no_std`-compatible and host-testable** (synthetic ELF byte
arrays, the way `kernel/src/elf.rs` already tests), leaving only syscall/mmap glue
arch- and QEMU-gated. This makes most of the loader testable with `cargo test`.

### D4 — Who maps the main program: VFS, not the loader (v1)

Two options:

- **A (MINIX-faithful):** VFS installs the *interpreter* as the executable and hands the
  loader an fd to the main program (`AT_EXECFD`); the loader maps the main program and
  its DSOs itself.
- **B (port-native):** VFS maps **both** the interpreter's and the main program's
  `PT_LOAD`s as `VR_FILE` regions (it already does exactly this per segment), passes the
  loader only the main program's entry point, and the loader maps **only the DSOs** —
  which it opens itself with ordinary `open()` + `mmap()`.

**Recommend B for v1.** It needs no new fd machinery (the port never installs an fd in
the target today), no `AT_EXECFD`, and it keeps the proven path (`vfs_memmap` per
segment) as the way images are mapped. The cost: the loader cannot apply per-segment
protections to the main program differently from the link-time ones, and `AT_PHDR` for
the main program is not available — and Phase 6 turned out not to need either, because it
is the *loader* that answers `dlopen` for a dynamic program rather than a static caller that
would have needed `AT_PHDR` to describe itself.

### D5 — Interpreter placement: fixed, non-PIE first

MINIX reserves 10 MB below the stack and lets `ld.so` relocate itself. For the port the
cheaper first step is a **non-PIE loader at a fixed high base** (below the user mmap
base: 4 GiB on x86/riscv, `0x30000000` on aarch64 — see the heap comment in
`minix-rt/src/lib.rs`). That removes the chicken-and-egg of the relocator needing
relocation. Cost: a fixed reservation (MINIX reserves 10 MB too, for the same reason),
and the loader is not itself relocatable until a later phase. Revisit if the loader ever
needs to be shared or ASLR arrives.

### D6 — Binding: eager

No lazy PLT, no resolver trampoline. `DT_BIND_NOW`-style: resolve every `JUMP_SLOT` and
`GLOB_DAT` before transferring control. Simpler, and it turns "symbol not found" into a
load-time error instead of a first-call trap.

### D7 — Library model, in three steps

1. **Classic non-PIE**: `ET_EXEC` main + a `ET_EXEC`-like fixed-base `.so`, so the loader
   needs only `GLOB_DAT`/`JUMP_SLOT` symbol binding — no `RELATIVE`. This is Phase 0 and
   is the smallest thing that is genuinely dynamic linking.
2. **PIC/`ET_DYN`**: DSOs and (optionally) the main program are position-independent,
   loaded at loader-chosen bases, so `RELATIVE` (and `R_*_64`/`IRELATIVE`) join the set.
3. **Sharing**: the actual memory win, and it largely exists already. VM's file page
   cache (`vm/cache.rs`) holds read-only file pages keyed by `(dev, offset)` with a
   `PhysBlock` reference, and `start_file_page` consults it before allocating
   (`cacheable = (!writable || shared) && file_off + page_size <= file_size`). So DSO
   `.text`/`.rodata` is shared across processes **if the loader maps those segments
   read-only**; a loader that maps a DSO read-write so relocations can be patched in
   place gets a private copy of every page and no sharing. `VR_SHARED` is a different
   thing (fork/COW semantics for `MAP_SHARED` pages, `vm/cow.rs`) — it is not the
   sharing mechanism. Phase 5 is measurement plus this mapping discipline, not new VM
   code. (The `FILEMMAP.md §3/§6` "block cache is a stub" claims predate the cache and
   should be corrected there.)

### D8 — TLS: single-module static TLS, placed by the loader

`init_tls()` copies one image (`__tls_start..__tls_end`) into a per-thread block; there is
no module id. v1 kept DSOs **without `PT_TLS`** and made the loader reject one that had it
(loudly, not silently).

**As built (Phase 3) — the loader places one module, which is all the port's runtime can
hold.** `minix-libc`'s `errno` and pthread handle are `#[thread_local]`, so `libc.so`
arrives with a `PT_TLS` whatever the design says, and the compiler reaches it through
`__tls_get_addr` (the local-dynamic form, since the symbols are local to the object). So
the loader now:

- accepts `PT_TLS`, and at most **one** object carrying it — the program's own if it has
  one (a program linked against the *rlib* carries the library's, which is the common
  case), otherwise a single loaded object's. Two is refused by name;
- builds the block at `tp - align16(p_memsz)` (variant II, the convention
  `tls_block_alloc` already uses), copies the initialised part, zeroes the rest, and
  installs the thread pointer **before** any initialiser runs — an initialiser may touch a
  thread-local;
- defines `__tls_get_addr`, which returns `tp - align16(p_memsz) + ti.offset` for this
  thread, reading `tp` from `[tp]` (the port's own convention) so a thread
  `pthread_create` started gets its own storage;
- resolves the three symbols a `cdylib` leaves undefined because it is linked with no
  script — `__tls_start`, `__tdata_end`, `__tls_end` — from that object's `PT_TLS`
  (§6.6);
- resolves `DTPMOD64` to a constant module id.

Out of scope, and refused rather than mishandled: two modules with TLS (the second's
storage would alias the first's), and `DT_TLSDESC`/`TPOFF64` (neither object needs them,
and initial-exec would be a second relocation path to maintain).

### D9 — Auxv: registers first, auxv only when a consumer needs it

The only loader inputs v1 has are (a) the main program's **entry point** and (b) the DSO
**search path** (compiled in). So v1 passes the entry point via a register — MINIX's
`_rtld_start(fct, ObjEntry, ps_string)`, minus `ps_string` — and needs **no auxv at
all**. The port builds the stack in the *kernel*, and has no `ps_strings`; neither is
needed for this. Auxv becomes necessary only for: a PIE main program whose base is
chosen at run time (`AT_BASE`), `AT_PHDR`/`PHENT`/`PHNUM` for a loader that maps the main
program (option A), or `LD_*` semantics. Add it in Phase 2 with a real consumer, not
speculatively.

### D10 — Determinism and ABI posture

No ASLR, deterministic bases, no symbol versioning, no `ldconfig`. Search path
`/lib:/usr/lib` plus `LD_LIBRARY_PATH`. Optional RELRO later; not v1.

## 6. Interface changes (exact)

### 6.1 `SYS_EXEC_LOAD`: carry the main program's ELF header page (ELF arm)

The 64-byte message (`vfs/exec.rs`, `kernel/src/system.rs::do_exec_load_handler`) is
nearly full: on the ELF arm, bytes **56..64 are free** (that field, `EXEC_LOAD_PATH_PTR_OFF
= 56`, is wasm-only and already `#[cfg]`-gated that way).

Use it for the **VA of the main program's ELF header page** when `PT_INTERP` is present (0
for a static image). *Revised from the first draft, which sent `e_entry`:* the loader also
needs `PT_DYNAMIC`, and one header VA yields both `e_entry` and the program headers — one
value instead of two registers, and it is the `AT_PHDR`-shaped fact Phase 2 will want
anyway. The page is **not** inside any `PT_LOAD` (the first segment starts at file offset
`0x1000`), so VFS maps it read-only at the image's link base, `min(p_vaddr - p_offset)`,
through the main program's own `vmfd`. An image whose first `PT_LOAD` does start at offset 0
already maps it and nothing extra is mapped.

- `servers/src/vfs/exec.rs`: new `EXEC_LOAD_MAIN_HDR_OFF: usize = 56`
  (`#[cfg(not(target_arch = "wasm32"))]`).
- `kernel/src/system.rs::do_exec_load_handler` + `kernel/src/syscall.rs::exec_elf_for_target`:
  accept it and forward to `exec_init_regs`.

### 6.2 `exec_init_regs`: the loader register convention

`hal::exec_init_regs(frame, entry, sp, argc, argv)` gains a sixth argument, `main_hdr`: the
main program's ELF header page VA, or 0 for a static image. Per arch, mirroring the shape of
MINIX's `_rtld_start(fct, ObjEntry, ps_string)`:

| Arch | `fct` (loader entry) | `main_hdr` | file |
|---|---|---|---|
| x86_64 | already `rip` (frame offset 160) | `r9` (offset 56) | `arch-x86_64/src/hal.rs` |
| riscv64 | `sepc` (offset 0) | `a2` (offset 96) | `arch-riscv64/src/hal.rs` |
| aarch64 | `ELR_EL1` (offset 256) | `x3` (offset 24) | `arch-aarch64/src/hal.rs` |

The exact register is a free choice (the loader is ours); the constraint is only that it
does not collide with what the existing `_start` reads (`[sp]` = argc, `[sp+8]` = argv).
The kernel **always writes** the slot, 0 included: the exec frame (`p_reg`) is not
guaranteed zeroed, so a stale register must not be what the loader reads as its argument.
riscv64 and aarch64 write it too — the plumbing stayed uniform from the start, and since
Phase 7 built a loader for each, all three consume it.

The loader's own `_start` must also **restore the stack pointer** before entering the main
program: `crt0` reads `argc`/`argv` from `(%rsp)`, and a call frame of the loader's left on
top of the exec'd stack would be read as `argc` (`crates/ldso/src/bin/ldso.rs`).

### 6.3 `pm_exec`: the `PT_INTERP` branch

In the ELF arm, after reading the headers of the path VFS was given:

1. Scan `PT_INTERP`; if absent, the current path runs unchanged.
2. If present, `req_read` the interpreter path (a NUL-terminated string in the image, not
   necessarily on the header page), look it up (`eat_path`), and parse **it** as an image —
   while also mapping the **main** program's `PT_LOAD`s (option B, D4). That means two
   `vmfd`s and two `dev`/`ino`/`file_size` sets; `vfs_memmap` is simply called once per
   image. A loader that itself names a `PT_INTERP` is refused (`ENOEXEC`): the register
   convention carries one header page.
3. `code_start`/`code_end` = the **union** of both images' `PT_LOAD` extents and the header
   page (the kernel clears exactly this in the fresh table; the gap between the two bases
   being cleared is harmless).
4. `EXEC_LOAD_ENTRY_OFF` = the **interpreter's** entry; `EXEC_LOAD_MAIN_HDR_OFF` = the main
   program's header page VA.
5. `finish_exec` returns the interpreter's entry as `pc`.

`MAX_EXEC_SEGS = 8` bounds each image's segments; the interpreter is small. The two
`vmfd`s are tracked separately: a failure before mapping closes whichever fd no region has
taken, and on success VM owns every fd a region took (it closes the last one to die).

Phase 0 also implemented `RELATIVE` in the loader (Appendix A) although its one object
carried no relocations — any object with a data address needs it. Phase 1 gives each object
one (a `const` pointer's initialiser), so the gate exercises it rather than trusting it.

### 6.4 Image manifest: a loader and a `.so` list

**As built — an env-gated injection rather than new manifest lists.** These files are built
by a toolchain an ordinary image build must not depend on, so they go in the way
`MINIXFS_EXTRA` does, under their own variable:

- `DYNLINK_BINS='dest=path;…'` (`crates/kernel/build.rs`) — read when set, injected into
  **both** images (which answers open question 5), with `cargo:rerun-if-env-changed` so
  turning it off restores an artifact-free image. Destinations must start with `/bin/`,
  `/sbin/`, `/lib/` or `/libexec/`; `MSYS2_ENV_CONV_EXCL` covers the Windows conversion.
- The gate sets it to `/libexec/ld.so`, `/lib/libdyn.so`, `/lib/libdyn2.so` and
  `/bin/dynhello`, and (Phase 3) `/lib/libc.so` and `/bin/dynclib`. The objects keep the
  names their `DT_NEEDED` entries use, so no soname symlink is needed: the loader's search
  path is literal (`crates/ldso/src/rtld.rs`).
- `boot-image` gains `/lib` and `/libexec`: unconditionally in the initramfs (`cpio.rs`,
  part of the base layout, because the loader must be reachable before `mount_root`) and in
  the MinixFS image only when a file needs one, created *after* every other directory so no
  existing inode is renumbered (`minixfs.rs`; the boot test pins `/devices`).

`Justfile` gets `dynlink-x86` (build) and `test-dynlink-x86` (gate).

### 6.5 Toolchain

- **No fork change for Phases 0–3.** As built, the C `.so`s are `clang -fPIC` objects
  linked with `lld -shared -soname libdyn.so` — no script of our own, LLD's defaults place
  `.dynsym`/`.dynstr`/`.hash`/`.rela.*` correctly. An executable is linked with the fork's
  stage1 `rustc` as the driver and requires `-Bdynamic -l:<name>.so
  --dynamic-linker=/libexec/ld.so`. `-Bdynamic` is not optional: the minix target's
  `crt_static_default` makes rustc pass `-static`, under which LLD refuses the `.so`
  altogether.
- The loader has **its own link script** (`tools/minix-ldso.ld`, base `0x04000000`):
  `tools/minix-user.ld` pins `. = 0x01000000`, and an explicit assignment wins over
  `--image-base`, so that flag cannot move it.
- **Phase 3 needed no fork either.** `crates/minix-libc` is a Rust library, so its shared
  object is built by rustc rather than LLD — `cargo rustc -p minix-libc --crate-type
  cdylib` — and three things had to be arranged for that to work
  (`tools/build-dynlibc.py`):

  1. **A PIC/`cdylib` target of the fork's own: the `-elf` triples**
     (`compiler/rustc_target/src/spec/targets/*_minix_elf.rs`, registered in
     `rustc_target/src/spec/mod.rs`). `relocation-model: pic` and
     `dynamic-linking: true`, plus `crt-static-allows-dylibs: true` — without that last
     flag rustc *silently drops* the `cdylib` crate type when `crt-static-default` is on,
     which is what the minix target sets
     (`rustc_session/src/output.rs::invalid_output_for_target`): the object was built as an
     rlib and no `.so` appeared. The target also **drops** the built-in minix target's
     `pre_link_args --image-base=0x1000000`: an object is mapped at `slot + p_vaddr`, so a
     non-zero link-time base would put it that far past the slot reserved for it —
     `rtld.rs` reserves the object's *highest* vaddr (`image_extent`'s `hi`) rather than
     its extent for the same reason. **The suffix is `-elf` and not `-dyn`** for a reason
     worth knowing before "fixing" it (§8).
  2. **That target's sysroot holds a PIC `core`/`alloc`, and that is *why* it has to be a
     target.** A `-C relocation-model=pic` on the crate cannot fix the *precompiled*
     `core`: the link fails with `R_X86_64_64 cannot be used against local symbol` until
     the `core` it links against is PIC. `tools/rust-config.py` therefore lists the `-elf`
     triples in `config.toml` **with `no-std = true`**, which makes bootstrap build those
     two crates and no std into their sysroots — 7 files where the static target has 51.
     That is the set `-Z build-std=core,alloc` used to produce, now built by the same
     stage1 compiler as everything else, with no nightly and no separate
     `CARGO_TARGET_DIR`. Rows 1–3 are therefore no longer a toolchain the port *lacks*;
     `just bootstrap` (and the pinned CI toolchain release) produce them.
  3. **`--features so` and `link-arg=--soname=libc.so`.** A `cdylib` is a final artifact
     and nothing downstream supplies the `panic` lang item, so the feature adds one
     (`crates/minix-libc/src/lib.rs`); the soname is what makes the program's `DT_NEEDED`
     the name the loader searches for.

  The program is then linked with `--allow-shlib-undefined`: the object leaves four
  symbols for the loader on purpose (§6.6), and LLD checks a linked object's undefined
  symbols by default.

### 6.6 What the loader gives the objects it loads

The loader is linkable to the objects it maps, the way a system `ld.so` is
(`rtld.rs::loader_defined`), and it answers five names that are in no object's tables:

- `__tls_get_addr` — the runtime linker's own, since the address of a thread-local
depends on which thread is asking (D8).
- `__tls_start`, `__tdata_end`, `__tls_end` — the TLS bounds a *statically linked*
program gets from `tools/minix-user.ld`. A `cdylib` is linked with no script of ours, so
it leaves all three undefined, and the object's `PT_TLS` is the better answer than a
second script would be: it is the same fact, stated once, and the loader has to read it
anyway. (It is also why these must be answered *per referencing object*: a non-PIE
executable exports its own `__tls_start` for its own, unrelated block, and a naive global
lookup would bind the object's reference to the program's.)
- On AArch64 a thread-local is reached through a *descriptor* instead, because `TLSDESC` is
that target's default and the compiler emits it per access. The loader fills each
`R_AARCH64_TLSDESC`'s two words — its own `__tlsdesc_static`, whose whole answer is the
descriptor's argument, and the variable's offset from the thread pointer — so an access
through the descriptor reaches the same block `__tls_get_addr` would. The dialect's other
resolver, the one that looks a *module* up for a symbol another object defines, is never
reached: the walk refuses such a descriptor when it has a symbol, for the one-module reason
the names above are answered per object.

## 7. Phased plan

Every phase is independently abandonable and has a gate. Phases 0–3 do not touch the
toolchain fork, and Phase 4 turned out not to need building at all; Phase 0 has no Rust at
the loaded-program end, which keeps the first result cheap to get.

### Phase 0 — classic non-PIE, end to end, x86_64, C only — **done**

The smallest thing that is really dynamic linking: an `ET_EXEC` main with `PT_INTERP` and
`DT_NEEDED`, and a fixed-base `.so`, both built with clang/LLD. The loader does symbol
binding (`GLOB_DAT`/`JUMP_SLOT`) plus `RELATIVE`; no TLS, no PIC main, no `dlopen`.

As built:

1. The `.so` needs no script of our own — `lld -shared -soname` (§6.5); the loader does
   (`tools/minix-ldso.ld`). Sources: `tools/libdyn.c`, `tools/dynhello.c`,
   `tools/build-dynlink.py`.
2. `crates/ldso`: `src/elf.rs` + `src/reloc.rs` are host-testable (11 tests), while
   `src/rtld.rs`/`src/bin/ldso.rs` sit behind the `bin` feature so host `clippy`/`test`
   skip the minix-only `_start`. `_start` reads the header-page register, relocates, and
   jumps with the exec'd `rsp` restored (§6.2).
3. `pm_exec` `PT_INTERP` branch + `SYS_EXEC_LOAD` field + `exec_init_regs` (§6.1–6.3):
   the register is added on all three hardware arches, the VFS branch and the loader are
   x86_64-only (D1).
4. `DYNLINK_BINS` injection + `boot-image` `/lib`/`/libexec` + `Justfile` recipes +
   `tools/smoke/dyn.tsv` (§6.4).

Gate (`just test-dynlink-x86`): boots, runs `/bin/dynhello`, and the harness matches the
whole line `dynlink-ok` on the serial log — a string that exists only in `/lib/libdyn.so`.
The negative check first asserts it is **absent** from `/bin/dynhello` (`grep -q`), which
is what makes the boot evidence about the loader rather than about the program.

### Phase 1 — PIC/`ET_DYN`, real bases, `RELATIVE` — **done**

- DSOs are PIC `ET_DYN` mapped by a deterministic base allocator, and the loader applies
  each object's `RELATIVE` (plus `R_X86_64_64`, `GLOB_DAT`, `JUMP_SLOT`).
- The main program stays non-PIE, so `AT_BASE`/auxv is deferred again (D9) — the choice the
  line above allowed.

As built:

- The base allocator and the image extent are pure address policy in
  `crates/ldso/src/layout.rs` (host-tested): the first `DT_NEEDED` lands at
  `DSO_BASE = 0x0200_0000`, each later one above the previous object's highest page plus a
  gap, and nothing is placed at or above `DSO_LIMIT` (the loader's own base). The gap is
  what makes an access past one object fault instead of landing in its neighbour.
- The relocation walk moved into `reloc.rs` behind a `RelocImage` trait (relocation table,
  symbol name, store), so it is host-tested against a synthetic `ET_DYN` image while the
  loader implements the same trait over real memory. A store is bounded to the object's own
  extent, so a relocation cannot write outside the object that owns it.
- The gate's program needs **two** objects, which is the only way the allocator is exercised
  in the guest: `libdyn.so` and `libdyn2.so` (one `RELATIVE` each) and three `JUMP_SLOT`s in
  the main. The strings live only in the objects, and the program's single output line holds
  one value per mechanism.
- `R_X86_64_COPY` is **not** implemented, and that is why the test program reaches libdyn's
  data through a function: a non-PIE executable's reference to a variable defined in a
  shared object produces a COPY. The loader refuses it with a message naming it rather than
  ignoring it, because an ignored COPY leaves the variable holding nothing.

Gate (`just test-dynlink-x86`, still one boot): `/bin/dynhello` prints
`dynlink-ok dynlink-data dynlink-2-ok` — a `JUMP_SLOT` into each object plus libdyn's own
`RELATIVE` — and each string is asserted **absent** from the program. Host side,
`cargo test -p ldso`: every relocation type the loader claims, applied once at its own
offset with the rest of the image unchanged; the base allocator and the extent; and the
refusal paths (unsupported type, unknown symbol, a target outside the image, a table size
that is not whole entries).

### Phase 2 — coverage, TLS, fork/exec — **done, less `LD_LIBRARY_PATH` and auxv**

- Full reloc/symbol coverage: `COPY` (reachable the moment a non-PIE program names a
  DSO's data), weak undefined symbols, and `DT_INIT`/`DT_INIT_ARRAY` ordering.
- A library's own `DT_NEEDED` is followed (transitively, one mapping per name).
- Static TLS across modules stays unimplemented, and is now **refused by name** (D8).
  (Phase 3 replaces that refusal with a loader that places the one module the port's
  runtime can hold.)
- `fork` after load and re-`exec` are exercised by the gate.
- Deferred with the reason: `LD_LIBRARY_PATH` (needs the loader to walk `envp`, which it
  now receives but does not read) and `AT_PAGESZ` (needs auxv, which is D9's work and so
  belongs with the frame changes in §6).

As built:

- `COPY` needed the walk to distinguish two scopes, so `reloc.rs` has a `Scope`: a
  `COPY` resolves with `Scope::ExcludeSelf` because this image's own symbol for the name
  *is* the destination. `RelocImage` gained `copy_range`, and a definition is only
  accepted when its `st_value .. + st_size` lies inside the object that states it.
- Weak undefined symbols resolve to 0 rather than failing the load; a strong one still
  fails it (D6). The referencing object's symbol bind is what decides, so the trait has
  `sym_is_weak`.
- Loading is a dependency-first DFS over `DT_NEEDED`, and a library already in the list is
  not mapped again — decided by the **file**, not by the `DT_NEEDED` spelling: first the
  resolved path (no syscall), then `st_dev`/`st_ino` from `fstat` on the descriptor just
  opened. Those are the reference's two checks (`libexec/ld.elf_so/load.c::_rtld_load_object`),
  and the name rule they need is `search.c`'s: a name containing a slash is a path, opened as
  it stands, and only a bare name is looked for under `/lib/`, `/usr/lib/`. The list is
  reverse topological, which is why initialisers walk it backwards.
- Initialisers are called with `argc`/`argv`/`envp`, so the loader's `_start` now reads
  `envp` off the exec'd stack.
- An object with `PT_TLS` is refused: the port's TLS is one module's, and silently
  letting an object read another module's storage is the failure this avoids.

Gate (`just test-dynlink-x86`, three steps in one boot):

| Step | Line | What only that step can show |
|---|---|---|
| `/bin/dynhello` | `dynlink-ok dynlink-data dynlink-2-ok dynlink-3-ok count=1` | bases, both objects' relocations, transitive load, cross-object resolution, initialisers, one initialiser run per object, and a third object of the program's own fitting the address space |
| `/bin/dynhello fork` | `child-ok …` | the mappings survive `fork`: the child calls into both objects and exits 0 |
| `/bin/dynhello exec` | `re-exec-ok …` | a second load of the same program, by the process the first one replaced itself with |

Each prefix can be produced by its own step alone — a whole-line match is anchored but
scans the whole log, so a prefix an earlier step printed would make a later step pass
without running.

The negative check still holds: none of the three strings is in the executable.

Host tests (`cargo test -p ldso`) cover each relocation type the loader claims — applied
once, at its own offset, with the rest of the image untouched — the scope each resolves
with, a weak reference resolving to 0 and the strong one failing, the allocator and the
`PT_TLS` query.

**Closed — and measuring it corrected the claim above.** The earlier text here said
"already loaded" was decided by the name a `DT_NEEDED` gave, and that a duplicate mapping
"would still print the right line". The first was a gap; the second was wrong.

`libdyn.so` now names `libdyn2.so` three ways in one link: the soname (which the loader has
to find on its search path), `/lib/libdyn2.so`, and `/lib/./libdyn2.so`. The last two are
*sonames* on link-only copies of the same object (`tools/build-dynlink.py::ALIASES`), so all
three names are one file with one inode and only the first is installed. The loader settles
them by the resolved path, then by `st_dev`/`st_ino`, and maps once.

The gate no longer takes that on trust. `libdyn2.so`'s initialiser bumps a counter in
`libdyn.so` — the one object the executable names once, so the count is about `libdyn2`'s
mapping and not its own — and `/bin/dynhello` prints it as `count=1` (the phase notes above
quote the gate line before this field existed). A second mapping would run the initialiser
twice and print `count=2`. The count travels through a call rather than an exported variable
because `dynhello` is non-PIE and an exported variable would be read through an
`R_X86_64_COPY` — a second place the count could live.

**What the duplicate does *not* do here, which was the more interesting result.** The
`count=2` case could not arise at all at the time: the second mapping did not fit. An address
space held **16 regions** (`crates/servers/src/vm/region.rs::MAX_REGIONS`); the two images'
`PT_LOAD`s plus the stack and the heap took **8** before the loader mapped anything, and the
loader maps **one region per `PT_LOAD`** — four for each object here (`R`, `R E`, `RW`, `RW`,
which is what an LLD `-shared` link emits). So a dynamically linked program fitted **exactly
two shared objects**, and both a third object and a second mapping of the first were refused
by VM with `EAGAIN`. Measured by adding a temporary diagnostic to `finish_mmap_file`: region
counts `9,10,11,12` while `libdyn2` is mapped, `13,14,15,16` while `libdyn` is, and then the
refusal at `16`.

The failure was also misreported — `a DT_NEEDED is not a PIC object` for a full region table
— and both halves are fixed:

- `MAX_REGIONS` is **32**, which is `8 + 4n <= 32` — *6* objects, the slack a program that
  carries a library of its own needs. Raising it is not the blunt move it looks: two thirds of
  a `VirRegion` was a 16-frame inline array, written in *fault* order and read by page offset,
  so it only agreed with itself when a region's pages faulted in address order — and nothing
  ever read it (`phys_at` had no callers). Dropping it takes a region from 192 bytes to 64, so
  a 32-entry table is *smaller* than the 16-entry one was: VM's `Vmproc` table loses ~230 KiB
  of BSS while gaining the room. What is left is `npages`, which the `VMIW_REGION` leak probe
  reads, now counting a region's pages instead of saturating at 16.
- The loader maps a segment through `minix_rt::vmem::mmap_status`, which carries VM's errno
  where `mmap` collapsed every failure to `MAP_FAILED`. `EAGAIN` is now
  `ld.so: no room in the address space for another object`, and anything else is reported as
  the error it is.

The gate has the third object (`tools/libdyn3.c`, `libdyn3.so`, printed as `dynlink-3-ok`),
which is what makes the budget measured rather than argued: nothing depends on that object and
it depends on nothing, so its arrival is about the address space and not about resolution. With
`MAX_REGIONS` back at 16 the gate fails — `ld.so: no room in the address space for another
object`, and no `dynlink-3-ok`.

So the de-duplication is proved by `count=1` (the property as an assertion, which is what would
catch a loader that mapped twice on an image with regions to spare), and the *budget* is proved
by the third object loading at all.

**Later, when the budget had to grow for graphics.** A Wayland compositor wants a dozen objects
(EGL, GLES, libdrm, gbm, xkbcommon, libinput, …), which is past both ceilings above, so two
things moved rather than one. First the *per-object* cost: an LLD `-shared` object carries a
`GNU_RELRO` `PT_LOAD` of its own, and nothing in the OS reads that header — not the loader, not
VFS's exec, not the kernel — so the segment cost a region per object for a protection never
applied. `-z norelro` (`tools/lld.py::NO_RELRO`) merges `.data.rel.ro` and `.got` into the
writable segment they are already writable in, so **4 regions an object became 3** — measured on
all three arches. `--no-rosegment` would make it 2, by moving the object's read-only data into
the executable segment; declined for now, because that maps `.rodata` and `.dynstr` executable.
Second, the cap: `MAX_REGIONS` 32 → **64**, which is `8 + 3n <= 64` — **18** objects — at ~576
KiB more BSS (`Option<VirRegion>` is 72 bytes, times `NR_PROCS`), plus the loader's
`MAX_OBJECTS` 8 → 24 so that it is not the binding ceiling instead. The third object below is
what measures it; the pin for the loader's own table is a `const _: () = assert!(…)` in
`rtld.rs`.

### Phase 3 — toolchain, and a dynamic C library — **done, less the soname scheme**

- `minix-libc` builds as `libc.so` with a soname, so **C programs** link dynamically —
  which is where `bash` could eventually shrink.
- Building it needed a PIC/`cdylib` target, which first came from a JSON spec plus
  `-Z build-std` and is now the fork's own `-elf` triple (§6.5). The loader gained
  single-module TLS (§6.6, D8) and the loader-defined symbols the object leaves undefined.
- Three things the loader turned out to assume, found by building a real library against
  it rather than a two-function test object (each is now a fixed bug, recorded here
  because the same assumptions are easy to reintroduce):

  1. **A symbol scan capped at 256 entries.** `libc.so` exports ~400 dynamic symbols, so
     every name past the cap was invisible — `stdout` failed to resolve. The cap is gone;
     an object's symbol count is now its `.hash` `nchain` clamped to the symbols that fit
     inside the object, which is the bound that does not go stale.
  2. **Refusing a main program with `PT_TLS`.** A program linked against the `minix-libc`
     *rlib* carries the library's thread-locals itself, so that refusal rejected the
     port's own Phase 0–2 test program. The program is now placed like any other object.
  3. **LD_LIBRARY_PATH** stays unimplemented: the loader receives `envp` (it passes it to
     initialisers) but does not walk it.

As built:

- `tools/build-dynlibc.py` builds `libc.so` (`cargo rustc --crate-type cdylib` under the
  dyn target, `--features so`, soname `libc.so`) and `tools/dynclib.c` linked against it,
  and `dynlink-x86` runs it after `tools/build-dynlink.py`.

    Those two objects moved when dynamic linking became something an image *has* rather
    than something only its gate built: the script now writes them into the release
  directory (`target/<triple>/release/`, the way `tools/build-c-hello.py` places
  helloc/ctest), `BOOT_BINS` carries `/lib/libc.so` and `/bin/dynclib` alongside
  `/libexec/ld.so`, and every image-assembling recipe depends on `just dynlib-<arch>`.
    The gate injects only the loader's own test objects now (`libdyn*.so`, `dynhello`), and
    its `/bin/dynclib` step therefore exercises the *shipped* program. See D2 above.
- The loader's TLS work is `rtld.rs::install_tls` + `__tls_get_addr` + the loader-defined
  bounds symbols; `reloc.rs` gained `R_X86_64_DTPMOD64` (the walk writes a module id, not
  a symbol value) and `layout.rs` the block-size rule, both host-tested.
- `crates/minix-libc`'s `minix_libc_tls_init` is a no-op under the `so` feature: for a
  dynamically linked program the loader owns the thread pointer, and installing a second
  block would move it away from the storage `__tls_get_addr` hands out. `crates/minix-libc`
  needs no other change, so the static library is untouched (D2).

Gate (`just test-dynlink-x86`, four steps in one boot): the three `dynhello` steps above,
plus `/bin/dynclib`, which must print
`libc-dyn-ok errno=2 msg=No such file or directory ctor=1`. The image the gate boots is a
*standard* one (`just build-x86`) plus the loader's test objects injected through
`DYNLINK_BINS`, because the loader, `libc.so` and `dynclib` are `BOOT_BINS` now — so this
step measures what an image ships, not a copy the gate built for itself. The line is the
whole chain in one place: the program asks `open` for a path that cannot exist (a `libc.so`
symbol), reads `errno` (its thread-local, so through the loader's `__tls_get_addr`), prints
`strerror`'s answer (a pointer into a buffer inside `libc.so`), and reports whether its
own constructor ran (`ctor`). The gate fails if the executable contains the message, so
the message can only have come from the object, and it fails if `ctor` is 0 — the
constructor was added precisely because the gate could not previously see the failure
below. The static `helloc`/`ctest` gates are unchanged and still pass (`just test-arches`).

**A dynamically linked program's own `.init_array` — found here, then fixed.** `crt0`
used to call `__minix_init_array`, which in a dynamically linked program resolves into
`libc.so` and walks *that object's* array — empty, because LLD synthesises
`__init_array_start`/`__init_array_end` for a `-shared` link and binds them inside the
object. The program's own constructors were therefore skipped without a word. `crt0` now
walks its own bounds itself (all three arches), and the helper is gone rather than left
as an internal whose default meaning is a trap. The gate's `ctor=1` field is what makes
that walk observable: skipping it in `tools/crt0-x86_64.S` turns the line into `ctor=0`
and fails the step — which is how the fix was checked, not just its green result.

**Known gaps.**

- **A worker thread's block is the object's, not the loader's.** `pthread_create` still
  goes through `tls_block_alloc`, which is sound only because the loader rounds `p_memsz`
  the same way and the layout is a single module — so it agrees. With a second TLS module
  (refused) that reasoning would not hold.
- **No soname/versioned symlink scheme** (the MINIX `shlib_version` idea): one version,
  named by the soname the object states, and the loader's search is literal. Nothing needs
  a version yet, so the idea is deferred rather than designed.

### Phase 4 — Rust: decided, and not built — **dropped**

The draft offered three shapes and said to choose after Phase 3 showed what the C ABI
surface costs. That cost is now known, and so is the benefit — and the benefit is not
there. **Recommendation: drop the phase.**

The premise was that every `userland` binary carries its own copy of
`minix-rt`/`minix-std`. It does not, in the way that matters. `tools/rc-cost.py` attributes
a linked binary's symbols to the crate that defines them (Rust's v0 mangling names it), and
`llvm-size` gives the total (release, x86_64):

| binary | `.text` | symbols | instantiated generics | layer functions | the program |
|---|---|---|---|---|---|
| `echo` | 994 | 957 | 0 | 117 | 840 |
| `cat` | 8,296 | 7,711 | 0 | 6,272 | 1,439 |
| `ls` | 21,103 | 19,914 | 7,850 | 7,031 | 5,033 |
| `sh` | 69,455 | 63,528 | 7,850 | 10,340 | 45,338 |
| `mfs` | 46,394 | 122,586 | 0 | 8,071 | 114,515 |
| `vfs` | 103,169 | 710,014 | 789 | 55,586 | 653,639 |

(Symbols include data and bss, which is why `mfs` and `vfs` exceed their `.text` at all — so
the layer column is an upper bound, not a floor.)

Three things follow, and each is enough on its own:

1. **There is no per-binary layer blob to remove.** `echo` is 994 bytes of `.text`
   *altogether*; the layer's object code — `minix-std`'s rlib alone is 465 KB — is not
   carried around, because the linker takes the members a binary references and `-O2`
   inlines most of those into the caller. What a shared object could share is only the
   functions that were *not* inlined: the "layer functions" column, a few KB per binary —
   and much of `vfs`'s 55 KB is the layer's data and bss, which a DSO would not share
   either.
2. **Instantiations can never be shared**, whatever the linking scheme: a generic or
   `#[inline]` body is compiled into every binary that calls it, which is the `_RI...`
   column.
3. **The duplication the draft worried about is already gone.** Two processes running the
   same binary share its text today — exec maps read-only `VR_FILE` regions and VM consults
   its file page cache before allocating a frame
   (`crates/servers/src/vm/mod.rs:1251`). "N processes running the same tool cost N copies
   in RAM" is not true of text (§2.2).

Against that, the cost is exactly what Phases 0–3 avoided. A `dylib` cannot be built for
the minix target at all — `cannot produce dylib for minix-std as the target
x86_64-pc-minix does not support these crate types` — and the `-elf` targets Phases 0–3
added are the *loader's* side of that, not the userland's: a Rust dylib needs the userland
itself built for a spec with `dynamic_linking: true` and `crt_static_allows_dylibs`, which
is `relocation_model: pic`, so every existing binary changes with it — or a second sysroot
for the whole userland, the image build and the stage1-verification flow included. Then:
the `panic` lang item in the new object (the same
problem Phase 3's `cdylib` had), `-C prefer-dynamic` plus `PT_INTERP` and the loader on
*every* userland binary, and the Rust-dylib caveats — the ABI is pinned to one compiler
build, the symbols are mangled and hashed, and only the non-inlined items are shared at all.

What would change the answer is a Rust userland shaped like a `libstd`: a large binary whose
program code is a small fraction of it. Nothing in the port is, and `tools/rc-cost.py` is
the check (give it a binary) — the same instrument Phase 5 needs. If the answer does change,
option (a) still buys nothing by construction and option (b) is what Phase 3 delivered for C.

### Phase 5 — sharing: measure, and keep the mapping discipline — **done**

The phase asked two things: that a read-only DSO page be one physical frame for every
process mapping the object, and that the loader keep it read-only. The second holds and
the first is now measured — but the first *reading* of the measurement was wrong, so both
are recorded here.

**The loader's discipline holds.** `crates/ldso/src/rtld.rs::read_and_map` maps each
`PT_LOAD` with `PROT_READ` (`| PROT_EXEC` for `PF_X`) and adds `PROT_WRITE` only for
`PF_W`, so the writable `.data`/`.got` — where the relocations go — is the private part.
Measured: every read-only page of `libc.so` in a live process has a PTE with no `RW` bit.
The last partial page of a read-only segment being private
(`file_off + page_size <= file_size` fails there, one page per segment) is expected, and
the probe shows exactly that for the main program's own text.

**Sharing works for two lives started together.** `tools/dso_share_probe.py` (recipes
`just probe-dso-share-x86` and `-riscv64`; `--arch` picks the page-table walk) boots the
dynamic image, runs `/bin/dynclib hold | /bin/dynclib hold` — two lives of the same dynamic
image, `libc.so` mapped by the loader in each — and walks **both** processes' page tables from
*outside* the guest (only the kernel knows a virtual address's frame, and the loader is the
subject under test). Result: **every read-only page is one physical frame set** — 13 of 13 on
x86_64, 11 of 11 on riscv64 — with the 4 writable pages private on both, as designed. A shared
object's text is therefore paid for once however its users start, which is the point of the
whole phase.

**Two fills of one page are one fill.** The first run of that probe, without any change to
VM, reported every page private: a page's first fault is a *fill*, and nothing joined a
fill already in flight, so two lives that reached a page at the same time each missed and
each allocated privately. Measured at the time — with two cold lives the cache took **no
hits at all**, with three it took 13, the later one arriving after the fills — and the same
shape C MINIX has (`mappedfile_pagefault` allocates per fault; only
`find_cached_page_byino` can hit). The port now joins them: `vm/vfs_request.rs::park_page`
parks a fault on the fill of the same file page that another process already has in
flight, and `release_parked` maps that frame into the waiter and lets it carry on. Only
*cacheable* fills are joinable — a writable `MAP_PRIVATE` page's frame is its owner's and
may be modified — which is the same predicate that decided the page could be shared at all.
Parking is keyed like the cache, on `(dev, ino, file offset)`, and a parked fault is
dropped when its process exits (`forget_parked`), so a completion can never map into a
freed address space. It also *reduces* the demand on the VFS request pool: two faults on
one page are one FDIO.

**What went wrong the first time, recorded because this work invites it.** The first run
was read as "the cache is empty", and its cause assigned to `cache_insert` refusing
`dev == 0` (C's `NO_DEV`) while this port mounts the root filesystem with device 0. Both
halves were wrong. The host-side code that read the cache out of guest memory decoded a
*non-page-aligned* address without honouring its offset within the page, so it read the
wrong bytes — and the port's file vnodes carry `dev == 0x9e7`, not 0, so the guard never
fired and the cache was populated all along. The probe now checks its walk against the
program text before believing anything, which is the self-check the first version needed.

**One real fix came out of it: the cache key had no inode in it.** The lookup matched
`(dev, dev_offset)` and only *restamped* `ino` (`vm/cache.rs::find_slot`), while
`dev_offset` for a file page is a **file offset** (`start_file_page` passes
`region.file_offset_at(page)`). `dev` is one filesystem's number, so two files' pages at
the same offset shared a key — every binary's page 0, for one — and once hits happen a page
of the wrong file can be handed over. C keys the file-fault path with
`find_cached_page_byino(dev, ino, offset)` for exactly this reason
(`.refs/minix-3.3.0/minix/servers/vm/mem_file.c:104,207`). The port now has both forms:
`cache_find_byino`/`cache_insert_byino` for file pages and the `_bydev` pair for the block
paths, with a test that two files at the same offset stay distinct. The file form also
stops treating device 0 as "no device" — the port's `NO_DEV` is `0xffff`, so C's guard was
refusing a device number this port really uses. Nothing sends
`VM_MAPCACHEPAGE`/`VM_SETCACHEPAGE`, so the block form is behaviourally unchanged and has
no caller outside the tests.

Gate: a measured assertion that two processes' `.so` text maps to one physical frame set
(not a comment), on at least two arches. **Both pass.** x86_64: `just probe-dso-share-x86`,
13/13 read-only pages, 4 writable ones private, on each of three runs. riscv64:
`just probe-dso-share-riscv64`, 11/11 read-only pages, 4 writable ones private. The probe
walks the arch's own tree (`--arch`), and only the walk differs: the loader's `DSO_BASE`, every
program's `TEXT_BASE`, the process table's symbol and the `Proc` slot stride are shared. aarch64
has no recipe yet — its read-only bit (`AP[2]`) is inverted relative to these two, so it is a
change to the walk rather than a row in it.

### Phase 6 — `dlopen`/`dlsym` — **done, less unloading and multi-module TLS**

Landed when the consumer appeared (§6.9, the C graphics stack): a driver lookup is a
`dlopen` at run time, which no `DT_NEEDED` can express.

- `rtld.rs` keeps the object list, its length, the base allocator and the program's arguments
  in a `static` rather than in `run`'s frame, because `run` returns to the main program and a
  `dlopen` arrives long afterwards. `load_now` does for one object what the startup walk does
  for the graph: resolve its `DT_NEEDED`, place it, relocate the group (dependencies first)
  and run the group's initialisers.
- The loader exports the family without a `.dynsym` of its own. It is a static executable, so
  exporting would mean self-relocation; instead `loader_defined` answers `__rtld_dlopen`,
  `__rtld_dlsym`, `__rtld_dlerror` and `__rtld_dlclose` with its own addresses, exactly as it
  already answered `__tls_get_addr`, and `libc.so` calls through them. So a C program gets
  `dlopen` from `libc.so`, and a statically linked one gets the honest failure — the split
  §6.6 describes for the TLS bounds.
- `RTLD_LOCAL` and `RTLD_GLOBAL` are implemented rather than accepted and ignored: an object
  carries a group id and whether it is in the global scope, and a lookup is two passes — the
  owner's own group first, then the global scope — so an object's own definition of a name
  wins over a global one of the same name, and a local load stays out of
  `dlsym(RTLD_DEFAULT, …)`. `RTLD_LAZY` and `RTLD_NOW` ask the same thing here, because eager
  binding (D6) is a superset of what lazy promises.
- What it does not do: **unload** — `dlclose` is accepted and nothing is unmapped, so a second
  `dlopen` of the same file returns the same handle — and **multi-module TLS**, because the
  one module the loader places is laid out at startup for every thread. An object carrying
  `PT_TLS` is therefore refused by name, and that refusal is the gap §6.9 records for the
  graphics stack rather than something a caller can work around.
- The table is not locked: two threads in `dlopen` at once would race it. Nothing so far calls
  it from more than one.

Gate: `tools/dynopen.c` (`/bin/dynopen`), five steps in `tools/smoke/dyn.tsv` on all three
arches — a `RTLD_LOCAL` load reached through its handle and *not* through `dlsym(RTLD_DEFAULT,
…)`, a `RTLD_GLOBAL` one reached through `RTLD_DEFAULT`, `dlclose` leaving the object usable,
and the two failures (`tools/libtls1.c`'s thread-local, and a path that does not exist)
asserted on the loader's own message. `just test-dynlink-<arch>`.

### Phase 7 — riscv64 + aarch64 — **done**

Per-arch reloc sets (Appendix A) and the `exec_init_regs` register choice (§6.2). Prefer
landing each phase on x86_64 first, then porting, matching how the exec work went
(`FILEMMAP.md §5` needed three arch-specific fixes after the x86 version was green).

As built — riscv64 first, then aarch64, in that order:

- The relocation tables became data (`reloc.rs`'s `Relocs`): one per target, every number
  taken from the fork's LLVM psABI headers, and the walk kept out of the per-target code.
  RISC-V forced the split of `abs64` from `glob_dat` — it has no `GLOB_DAT`, and
  `R_RISCV_64` fills both roles — which is why `action` compares rather than matches.
- TLS placement is per target: the thread pointer is past its block on x86_64 and *at* it on
  the other two (`layout::TP_IS_PAST_THE_BLOCK`, matching the runtime's `tls_block_alloc`),
  and `read_tp` reads `fs:[0]`, `tpidr_el0` or `tp` accordingly.
- AArch64 reaches thread-locals through a `TLSDESC` *descriptor*, so the loader fills the
  pair and supplies the resolver (§6.6).
- `_start` per arch (§6.2) — `r9`, `a2`, `x3` — with the exec'd stack and the header page
  parked in callee-saved registers across the call, since the program's entry reads the
  three arguments back off that stack.
- Three bugs the x86-only gates could not have shown, all in the *exec* and *DSO-mapping*
  paths rather than in the loader's own rules, are recorded in §8: the page `.text` shares
  with `.rodata` losing its execute bit, the pre-fault workaround's dependence on a region
  *not* being executable, and the loader filling a DSO's `.bss` tail from the file.

Gate: `just test-dynlink-riscv64` and `just test-dynlink-aarch64`, the same arch-neutral
`tools/smoke/dyn.tsv` as x86_64 — four steps each, green.

## 8. Risks and traps

- **The toolchain fork is the schedule risk**, not the loader (§6.5, Phase 3/4).
- **Attribution / lazy exec pages.** `FILEMMAP.md §6` records the kernel-mode fault
  *attribution* gap (a kernel copy faulting a lazy page is attributed to the copier).
  The loader reads its own mapped DSO pages and relocates into them; keep every loader
  access a **user-mode** access, or the gap becomes a SIGSEGV that looks like a loader
  bug.
- **The exec pre-fault workaround.** `do_vfs_mmap`'s one-shot non-exec pre-fault
  exists because of the gap above. A second image (the interpreter, and later DSOs)
  must be considered by whatever that pre-fault logic assumes; read
  `Vmproc::prefault_exec` + `Fault::for_prefault` before adding images.
- **`MAX_EXEC_SEGS = 8` / `EXEC_HDR_MAX = 8192`.** Fine for the interpreter; do **not**
  reuse `pm_exec`'s header reader to parse a DSO — DSOs are the *loader's* problem and
  should be read/mapped by the loader, not VFS.
- **The region table, not memory, is what bounds how many objects a program can have.**
  `MAX_REGIONS` is **64 per process** (`vm/region.rs`) and the loader spends **one region per
  `PT_LOAD`** — **3** for each object this tree links, because `tools/lld.py::NO_RELRO` drops the
  `GNU_RELRO` segment an LLD `-shared` link otherwise emits and nothing in the OS reads that
  header. The two images' regions plus the stack and the heap take 8 before the loader runs, so a
  dynamically linked program fits **18** objects (`8 + 3n <= 64`) and the nineteenth `mmap` is
  refused (`EAGAIN`). The loader's own `MAX_OBJECTS` (`crates/ldso/src/rtld.rs`) is a *second*
  ceiling that has to move with it: it counts the main program, so 24 is the 18 objects plus the
  program and slack. It was **16 when dynamic linking landed** (two objects) and **32** when the
  Phase 2 gate landed (six). Two things to remember before raising it again: the cost is a
  constant times `NR_PROCS` (256), so it is the *size* of `Option<VirRegion>` — 72 bytes,
  measured — that matters rather than a per-page array; and the loader's table is a stack local,
  so `MAX_OBJECTS` carries its own pin (`rtld.rs`) against the 1 MiB user stack.
- **An `mmap` failure has to carry VM's reason, or the loader names the wrong thing.**
  `read_and_map` maps each `PT_LOAD` with `MAP_FIXED` and used to turn every refusal into
  `a DT_NEEDED is not a PIC object` — a message about the image for a full region table. That
  is why `minix-rt` has `vmem::mmap_status` (`Err(errno)` alongside `mmap`'s `MAP_FAILED`): the
  errno is what tells "the address space is full" from "this image cannot be mapped", and a
  loader diagnostic that cannot tell them apart sends the reader to the wrong file. Checked by
  putting `MAX_REGIONS` back to 16: the message is the one that names the address space.
- **RISC-V X bit / AArch64 address range.** Executable regions must reach `VR_EXEC` so
  RISC-V text pages get the X PTE (`FILEMMAP.md §5` Bug 4); `MAX_USER_ADDRESS` on
  aarch64 covers the whole TTBR0 range, so a bad loader base can become an eret-retry
  loop (`FILEMMAP.md §6`).
- **A page two `PT_LOAD`s share needs their *union*, and the obvious fix is the wrong one.**
  Phase 7 found `.text`'s last page — the one holding the `.plt` — losing `PTE_X`, because
  `do_vfs_mmap` trims the earlier region out of a page the later segment claims and the
  `.rodata` segment does not ask for X. Widening that segment's own protection fixes the
  fetch and is *still* wrong: a region carrying X is not pre-faulted, and a kernel-mode copy
  cannot fault a page in (the gap above), so the literal a program writes without reading
  first goes missing — invisibly, on x86_64 and aarch64, where nothing enforces the bit.
  The union therefore travels with `PROT_PREFAULT`, which is what lets a data region be
  executable and eager at once.
- **A DSO's `.bss` is not past the end of its file.** `mmap` fills a page from the file as
  far as the *file* goes, and what follows a `PT_LOAD` in a `.so` is LLD's symbol table, its
  string table and its section headers — so a segment's zero-filled tail has to be cleared
  explicitly (`rtld.rs::read_and_map`). The exec path gets this from the in-file end VFS
  sends (`VM_VFS_MMAP`); without it the tail reads as plausible-looking data until something
  follows it as a pointer, which is how it surfaced: libc's exit-handler list.
- **TLS silently wrong is worse than absent.** A DSO with `PT_TLS` loaded by a
  single-module TLS runtime would compute plausible-looking wrong offsets (D8). Reject
  it explicitly.
- **Don't touch the defaults.** Every gate must run the static path unchanged (D2); a
  regression in the static exec path is the worst outcome of this work.
- **`parse_elf_header` is stricter than `pm_exec`.** If the initramfs/boot path
  (`load_elf`) is ever handed a dynamic binary it will reject it (correctly). Don't
  "fix" it into accepting `ET_DYN` as part of this work; it is a different path.
- **Joining a fill adds a wait that C MINIX does not have.** `park_page` blocks a process on
  another process's FDIO, so a cycle is conceivable that C cannot have: the filler's *other*
  thread waiting on the parked process while the parked process waits for the fill. VFS
  answers a fault quickly, and the same shape already exists within one process
  (`page_pending`), but a hang here would look like a wedged guest rather than a bug in a
  gate — reach for `tools/dso_share_probe.py` and VM's request pool first.
- **A console write keeps fewer bytes on riscv64 than on x86_64.** The probe's pace is per
  arch for a measured reason: on the riscv64 image a single write of *two* bytes keeps only
  its first, and one of six keeps about one in six, so 37 typed bytes have to go one at a
  time (`ARCHS["pace"]`). Measured with `echo 0123456789abcdefghijklmnopqrst`, every byte
  distinct; the whole line then arrives and runs, which is what `tools/smoke/dyn.tsv`'s short
  commands never exposed. A truncated line still *runs*, so a gate that checks only that
  something happened passes on a mangled command — the echo has to be checked whole.
- **The shared-object targets are `-elf`, not `-dyn`, and the reason is cc-rs.** They
  first landed as `<triple>-dyn`, which `just bootstrap` rejected before it built anything:
  `error occurred in cc-rs: unknown environment/ABI `dyn` in target
  `aarch64-unknown-minix-dyn``. `bootstrap` asks cc-rs for a C compiler for *every* target
  in `config.toml` (`utils/cc_detect.rs::fill_compilers`), and cc-rs parses a four-component
  triple's last component as an environment/ABI, refusing one it does not know
  (`cc-rs/src/target/parser.rs::parse_envabi`). `-elf` is the one it maps to *no*
  environment and *no* ABI, so the name is the executable triple plus a suffix and nothing
  else, matching the fork's existing `*-none-elf` spelling; `-gnu` would parse but claims an
  ABI this target has no glibc for. What the suffix may not do is vary the *OS*: the port's
  crates are `#[cfg(target_os = "minix")]`, so `minix` has to stay the OS component.
- **Doc/skill traps.** `silent-failure-traps` applies to the new gates: a scenario step
  whose input is sent before the prompt is dropped, and an expectation another step
  already printed makes a command that never ran pass.

## 9. Verification strategy

Three layers, matching `minix-testing`:

1. **Host `cargo test`** — the loader's ELF/`.dynamic`/`.dynsym`/relocation logic, driven
   by synthetic ELF byte arrays (the pattern `kernel/src/elf.rs` tests already use). This
   is most of the code and it runs with no QEMU. Property tests for the base allocator
   (no overlap, alignment).
2. **Host `cargo test` for the interfaces** — `pm_exec`'s interpreter branch
   (`servers` crate tests), the `SYS_EXEC_LOAD` layout assertions
   (`arch-common`/`servers` have precedent: `test_vfs_pm_messages`).
3. **QEMU gates** — `just test-dynlink-x86` (and `-riscv64`/`-aarch64`, both green since
   Phase 7), built
   on `tools/smoke/feed.sh` + a `tools/smoke/dyn.tsv`, asserting on the serial log. The
   negative assertion (§7 Phase 0 gate: the string is not in the main binary) is what
   makes the gate meaningful rather than a print that would pass either way.
4. **QEMU measurement, not a serial-log gate** — `tools/dso_share_probe.py` (recipes
   `just probe-dso-share-x86` / `-riscv64`) reads two processes' page tables out of guest physical
   memory, because only the kernel knows a virtual address's frame and the loader is the
   subject under test. It is what makes Phase 5's claim a measurement (§7 Phase 5). It
   drives QEMU itself rather than running a `tools/smoke` scenario, so it is not part of
   `test-arches` — run it alongside the gates when the loader or VM's file fault path
   changes.

Plus the existing suites must stay green on all three arches at every phase
(`just test-qemu`, `just test-boot`, the smoke scenario).

## 10. Open questions

1. ~~Does `VR_SHARED` actually share frames today?~~ **Answered.** Sharing is not
   `VR_SHARED` — that flag is fork/COW semantics for `MAP_SHARED` pages (`vm/cow.rs`) —
   it is the `vm/cache.rs` file page cache, which `start_file_page` consults and
   `finish_page` populates for read-only pages fully inside the file, and Phase 5 measured
   it sharing a DSO's read-only pages. It needed one fix to be *sound* — the key did not
   include the inode, so two files' pages at one offset shared an entry — not to be
   *enabled*. Residual questions: is a per-process writable `.data`/`.got` for each DSO
   acceptable (it is the standard cost, and it caps how much a `.so` actually saves); and
   is the first-touch race worth joining in-flight fills to remove? **Done in Phase 5**:
   `park_page`/`release_parked` join a fill already in flight, so two lives started together
   share. Residual: the joining is measured on x86_64 and riscv64 only (not aarch64), and it
   is a wait C MINIX does not have (§8).
2. ~~**Rust `std` strategy** (Phase 4): dynamic libc + static std, or dynamic `libstd`?~~
   **Answered by dropping Phase 4.** Phase 3 costed the C ABI surface and `tools/rc-cost.py`
   showed nothing in the port is shaped like a `libstd`, so std stays static and the shared
   object is the C library alone.
3. **Main program PIE or not?** Non-PIE is simpler (no `RELATIVE` in the main, fixed
   address as today) but a PIE main is the modern shape and gives the loader a reason to
   need `AT_BASE`. Recommend non-PIE through Phase 1. **As built: non-PIE** — the main is
   still `ET_EXEC` at `0x01000000` (D5) and `AT_BASE` is unused, so this stays an option the
   plan left open rather than a decision that was taken.
4. **`ps_strings`/`AT_SUN_EXECNAME`**: skip (no `ps`). **Confirmed**: neither name, nor
   `AT_PAGESZ`, appears anywhere in `crates/` or the minix `std` PAL, so there is no consumer
   to give them to (`AT_PAGESZ` would otherwise be the first auxv entry worth passing, D9).
5. **Where does the loader live when the root FS is not mounted?** The boot binaries come
   from the initramfs; confirm the loader must be in **both** the initramfs and the
   MinixFS image, or only the latter (early execs happen before `mount_root`).
   *Answered in Phase 0: both.* A dynamic binary can be exec'd before the root is mounted
   as easily as after, and `DYNLINK_BINS` injects into both images for that reason.

## Appendix A — ELF relocation sets

The set each arch must handle, by name. **The numeric values must be taken from the
arch's psABI header, not from memory** — copy them into `crates/ldso` beside the
`DT_*`/`PT_*`/`AT_*` constants, which in turn should be copied from the reference:
`PT_*`/`DT_*` from `.refs/minix-3.3.0/minix/include/sys/elf_common.h`, and `AT_*`
from `.refs/minix-3.3.0/sys/sys/exec_elf.h` (e.g. `AT_ENTRY = 9`,
`AT_SUN_EXECNAME = 2014`).

| Purpose | x86_64 | aarch64 | riscv64 |
|---|---|---|---|
| absolute word | `R_X86_64_64` | `R_AARCH64_ABS64` | `R_RISCV_64` |
| base-relative | `R_X86_64_RELATIVE` | `R_AARCH64_RELATIVE` | `R_RISCV_RELATIVE` |
| global data | `R_X86_64_GLOB_DAT` | `R_AARCH64_GLOB_DAT` | `R_RISCV_64` / GLOB_DAT |
| PLT/jump slot | `R_X86_64_JUMP_SLOT` | `R_AARCH64_JUMP_SLOT` | `R_RISCV_JUMP_SLOT` |
| copy | `R_X86_64_COPY` | `R_AARCH64_COPY` | `R_RISCV_COPY` |
| TLS module | `R_X86_64_DTPMOD64` | `R_AARCH64_TLS_DTPMOD64` | `R_RISCV_TLS_DTPMOD64` |
| TLS DSO offset | `R_X86_64_DTPOFF64` | `R_AARCH64_TLS_DTPREL64` | `R_RISCV_TLS_DTPREL64` |
| TLS TP offset | `R_X86_64_TPOFF64` | `R_AARCH64_TLS_TPREL64` | `R_RISCV_TLS_TPREL64` |
| TLS descriptor | — | `R_AARCH64_TLSDESC` | — |
| ifunc | `R_X86_64_IRELATIVE` | `R_AARCH64_IRELATIVE` | `R_RISCV_IRELATIVE` |

Dynamic tags needed: `DT_NEEDED`, `DT_STRTAB`/`DT_STRSZ`/`DT_SYMTAB`/`DT_SYMENT`,
`DT_HASH` and `DT_GNU_HASH`, `DT_RELA`/`DT_RELASZ`/`DT_RELAENT` (and `DT_REL*`),
`DT_JMPREL`/`DT_PLTRELSZ`/`DT_PLTREL`, `DT_SONAME`, `DT_RPATH`/`DT_RUNPATH`, `DT_INIT`/
`DT_INIT_ARRAY`, `DT_SYMBOLIC`/`DT_FLAGS`/`DT_FLAGS_1` (`DF_1_NOW`).

## Appendix B — bookkeeping

- `README.md` "Project Structure": add `crates/ldso` (**done**).
- `PORTING_PLAN.md`: a "Dynamic linking" phase entry pointing at this file (**done**, as a
  status entry rather than a phase: the work is a track of its own and Phase 4 was dropped).
- `Justfile`: `dynlink-x86` / `test-dynlink-x86` (**done**); `dynlink-riscv64` /
  `dynlink-aarch64` and their `test-` recipes (**done**, Phase 7, on the same
  `tools/smoke/dyn.tsv`); `probe-dso-share-x86` and `probe-dso-share-riscv64` (**done**,
  Phase 5's measurement — not in `test-arches`, see §9; `--arch` selects the walk, and
  aarch64's is still owed, §7 Phase 5).
- `.agents/skills/minix-boot-process`: a note that a dynamic exec enters `ld.so` first —
  a future boot-chain reader will otherwise conclude the wrong process started (**done**).
- Any `.rules` addition (per the hygiene policy, in the PR description, not inline). Two
  candidates from Phase 0: "the exec path has no `e_type` check while `parse_elf_header`
  does — don't conflate them", and "a new directory in `build_minixfs` renumbers every
  inode after it, which the boot test's `/devices` inode 7 depends on".
