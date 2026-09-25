# Dynamic linking for minixrs — implementation proposal

Status: **proposal — design settled, Phases 0–3 built, Phase 4 decided (dropped)** (on
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
- **The payoff is nearly free.** VM already caches and shares read-only file pages
  across processes (`vm/cache.rs`, consulted by `start_file_page`), so a DSO's `.text`
  is shared the moment a loader maps it read-only — no new VM code. Phase 5 is
  measurement plus that mapping discipline (§5 D7, §7 Phase 5).
- The kernel ELF loader needs almost no change: the **exec path never checks `e_type`**
  (`servers/src/vfs/exec.rs::pm_exec` reads `PT_LOAD`s directly), and DSOs are mapped by
  *userland* `mmap`, not by `parse_elf_header`. The one kernel-side edit is a register
  convention so the loader learns where the main program is (`main_hdr`, §6.1–6.2).
- Recommended shape: a **new Rust `ldso` crate** installed as `/libexec/ld.so`, an
  **opt-in** dynamic build (default stays static, exactly as MINIX does), and a
  **classic non-PIE first** progression so each phase adds exactly one mechanism.
- **wasm32 is out of scope** — exec there is host module instantiation and there are no
  faults to fault on (`ARCH_WASM32.md`). This is a three-arch project.
- **The toolchain fork was not needed.** The long pole was expected to be a dynamic/PIC
  variant of the `*-minix` targets in `rust/`, but a JSON target plus `-Z build-std`
  carries Phases 0–3 (§6.5). The fork's static target is untouched, so static stays
  exactly what it was (D2).
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

- **`dlopen`/`dlsym`** at runtime (Phase 6; the v1 loader is eager and load-only).
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

- **Shared text between processes.** Two processes running the *same* binary already
  share its text: exec maps read-only `VR_FILE` regions and VM looks its file page cache
  up before allocating a frame (`crates/servers/src/vm/mod.rs:1251`,
  `cacheable = (!writable || shared) && …`). So the DSO case is **different** binaries
  sharing one object's text — which is what a C library buys, and what Phase 3 delivered.
  What stays private per process either way is the writable, relocated part
  (`.data`/`.got`) and every monomorphised instantiation (`tools/rc-cost.py`), and that is
  what bounds the saving. See §2.3, Phase 4's decision and Phase 5.
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
the main program is not available — neither matters until `dlopen` (Phase 6), which is
where option A can be revisited.

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
riscv64 and aarch64 write it too although no loader is built for them yet (D1, Phase 7) —
the plumbing stays uniform and the value is simply never consumed there.

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

  1. **A PIC/`cdylib` target: `tools/minix-dyn-target/x86_64-pc-minix-dyn.json`.**
     `relocation-model: pic` and `dynamic-linking: true`, plus
     `crt-static-allows-dylibs: true` — without that last flag rustc *silently drops* the
     `cdylib` crate type when `crt-static-default` is on, which is what the minix target
     sets (`rustc_session/src/output.rs::invalid_output_for_target`): the object was built
     as an rlib and no `.so` appeared. The target also **drops** the built-in minix
     target's `pre_link_args --image-base=0x1000000`: an object is mapped at
     `slot + p_vaddr`, so a non-zero link-time base would put it that far past the slot
     reserved for it — `rtld.rs` reserves the object's *highest* vaddr
     (`image_extent`'s `hi`) rather than its extent for the same reason.
  2. **`-Z build-std=core,alloc`.** The target's sysroot `libcore` is not position
     independent, and the link fails with `R_X86_64_64 cannot be used against local
     symbol` until `build-std` rebuilds it under the PIC target. That, and not a fork, is
     what makes the toolchain the long pole: the fork's static target is never touched
     (D2), and the object's `core` is built into `target/dynlibc/` rather than into the
     sysroot the static userland links against.
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
- Loading is a dependency-first DFS over `DT_NEEDED` with a name-based "already loaded"
  check, so a diamond loads one mapping and a cycle terminates. The list is reverse
  topological, which is why initialisers walk it backwards.
- Initialisers are called with `argc`/`argv`/`envp`, so the loader's `_start` now reads
  `envp` off the exec'd stack.
- An object with `PT_TLS` is refused: the port's TLS is one module's, and silently
  letting an object read another module's storage is the failure this avoids.

Gate (`just test-dynlink-x86`, three steps in one boot):

| Step | Line | What only that step can show |
|---|---|---|
| `/bin/dynhello` | `dynlink-ok dynlink-data dynlink-2-ok` | bases, both objects' relocations, transitive load, cross-object resolution, initialisers |
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

**Known gap.** De-duplication is by the name a `DT_NEEDED` gave; two different names for
one file (a path and a soname) would map it twice. It is also not *isolated* by the gate:
a duplicate mapping would still print the right line, because both copies are resolvable.
The gate proves the loading order and the initialiser order; the single mapping is
correct by construction rather than measured.

### Phase 3 — toolchain, and a dynamic C library — **done, less the soname scheme**

- `minix-libc` builds as `libc.so` with a soname, so **C programs** link dynamically —
  which is where `bash` could eventually shrink.
- Building it needed a PIC/`cdylib` target and `build-std`; the toolchain fork was not
  needed after all (§6.5). The loader gained single-module TLS (§6.6, D8) and the
  loader-defined symbols the object leaves undefined.
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
- The loader's TLS work is `rtld.rs::install_tls` + `__tls_get_addr` + the loader-defined
  bounds symbols; `reloc.rs` gained `R_X86_64_DTPMOD64` (the walk writes a module id, not
  a symbol value) and `layout.rs` the block-size rule, both host-tested.
- `crates/minix-libc`'s `minix_libc_tls_init` is a no-op under the `so` feature: for a
  dynamically linked program the loader owns the thread pointer, and installing a second
  block would move it away from the storage `__tls_get_addr` hands out. `crates/minix-libc`
  needs no other change, so the static library is untouched (D2).

Gate (`just test-dynlink-x86`, four steps in one boot): the three `dynhello` steps above,
plus `/bin/dynclib`, which must print
`libc-dyn-ok errno=2 msg=No such file or directory ctor=1`. The line is the whole chain in
one place: the program asks `open` for a path that cannot exist (a `libc.so` symbol),
reads `errno` (its thread-local, so through the loader's `__tls_get_addr`), prints
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
x86_64-pc-minix does not support these crate types` — so it needs either the fork's target
spec (`dynamic_linking: true`, `crt_static_allows_dylibs`) or the whole userland build moved
onto a JSON target with `-Z build-std`, which is every `just build-*`, the image build and
the stage1-verification flow. Then: the `panic` lang item in the new object (the same
problem Phase 3's `cdylib` had), `-C prefer-dynamic` plus `PT_INTERP` and the loader on
*every* userland binary, and the Rust-dylib caveats — the ABI is pinned to one compiler
build, the symbols are mangled and hashed, and only the non-inlined items are shared at all.

What would change the answer is a Rust userland shaped like a `libstd`: a large binary whose
program code is a small fraction of it. Nothing in the port is, and `tools/rc-cost.py` is
the check (give it a binary) — the same instrument Phase 5 needs. If the answer does change,
option (a) still buys nothing by construction and option (b) is what Phase 3 delivered for C.

### Phase 5 — sharing: measure, and keep the mapping discipline

The sharing mechanism is already in the tree — `vm/cache.rs` is a real LRU file page
cache (4096 pages, `PhysBlock`-referenced), `start_file_page` looks a frame up in it
before allocating, and `finish_page` inserts read-only pages that lie fully inside the
file. So a read-only DSO page is shared across processes with **no new VM work**; this
phase is verification plus one loader constraint:

- **The loader must map read-only segments read-only.** `cacheable` requires
  `!writable`. A loader that maps a DSO `PROT_READ|PROT_WRITE` so it can patch it in
  place gets a private copy of every page and no sharing. Map `.text`/`.rodata`
  `PROT_READ` (`| PROT_EXEC`); leave the writable `.data`/`.got` private — that is where
  the relocations go anyway.
- **The last partial page of a read-only segment is private** (`file_off + page_size <=
  file_size` fails there). Expected, one page per segment.
- **The one thing to check first, before writing loader code**: confirm `cacheable` is
  actually reached for the path the loader will use. Today exec segments get there
  (`do_vfs_mmap` sets `VR_WRITABLE` only for `PF_W` segments), and a userland
  `mmap(PROT_READ)` does too (`finish_mmap_file`). If a measurement disagrees, that is a
  read of `start_file_page`, not a build.
- **Measure it**: two processes running the same dynamic binary must share the `.so`'s
  text frames. `VMIW_REGION`, the allocator probes (`tools/alloc_churn_probe.py`), and a
  cache-size query are the tools. `VM_MAPCACHEPAGE`/`VM_SETCACHEPAGE` exist for the
  filesystem's own block sharing and are not needed for DSO text.

Gate: a measured assertion that two processes' `.so` text maps to one physical frame set
(not a comment), on at least two arches.

### Phase 6 — `dlopen`/`dlsym` (optional)

Only if a consumer appears. Needs symbol lookup by name, `.init_array`/`.fini_array`
running at load, and — for a *static* caller to `dlopen` — either option A (D4) or an
`AT_PHDR` for the main program.

### Phase 7 — riscv64 + aarch64

Per-arch reloc sets (Appendix A) and the `exec_init_regs` register choice (§6.2). Prefer
landing each phase on x86_64 first, then porting, matching how the exec work went
(`FILEMMAP.md §5` needed three arch-specific fixes after the x86 version was green).

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
- **RISC-V X bit / AArch64 address range.** Executable regions must reach `VR_EXEC` so
  RISC-V text pages get the X PTE (`FILEMMAP.md §5` Bug 4); `MAX_USER_ADDRESS` on
  aarch64 covers the whole TTBR0 range, so a bad loader base can become an eret-retry
  loop (`FILEMMAP.md §6`).
- **TLS silently wrong is worse than absent.** A DSO with `PT_TLS` loaded by a
  single-module TLS runtime would compute plausible-looking wrong offsets (D8). Reject
  it explicitly.
- **Don't touch the defaults.** Every gate must run the static path unchanged (D2); a
  regression in the static exec path is the worst outcome of this work.
- **`parse_elf_header` is stricter than `pm_exec`.** If the initramfs/boot path
  (`load_elf`) is ever handed a dynamic binary it will reject it (correctly). Don't
  "fix" it into accepting `ET_DYN` as part of this work; it is a different path.
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
3. **QEMU gates** — `just test-dynlink-x86` (and `-riscv64`/`-aarch64` in Phase 7), built
   on `tools/smoke/feed.sh` + a `tools/smoke/dyn.tsv`, asserting on the serial log. The
   negative assertion (§7 Phase 0 gate: the string is not in the main binary) is what
   makes the gate meaningful rather than a print that would pass either way.

Plus the existing suites must stay green on all three arches at every phase
(`just test-qemu`, `just test-boot`, the smoke scenario).

## 10. Open questions

1. ~~Does `VR_SHARED` actually share frames today?~~ **Answered (this pass).** Sharing is
   not `VR_SHARED` — that flag is fork/COW semantics for `MAP_SHARED` pages
   (`vm/cow.rs`) — but the `vm/cache.rs` file page cache, which `start_file_page`
   consults and `finish_page` populates for read-only pages fully inside the file. So
   read-only DSO text is shared across processes already, and Phase 5 shrinks to
   measurement + mapping discipline. Residual question: is a per-process writable
   `.data`/`.got` for each DSO acceptable (it is the standard cost, but it caps how much
   a `.so` actually saves).
2. **Rust `std` strategy** (Phase 4): dynamic libc + static std, or dynamic `libstd`? Not
   decidable until Phase 3 costs the C ABI surface.
3. **Main program PIE or not?** Non-PIE is simpler (no `RELATIVE` in the main, fixed
   address as today) but a PIE main is the modern shape and gives the loader a reason to
   need `AT_BASE`. Recommend non-PIE through Phase 1.
4. **`ps_strings`/`AT_SUN_EXECNAME`**: skip (no `ps`). Confirm nothing in the port's
   `std` PAL wants them.
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
| ifunc | `R_X86_64_IRELATIVE` | `R_AARCH64_IRELATIVE` | `R_RISCV_IRELATIVE` |

Dynamic tags needed: `DT_NEEDED`, `DT_STRTAB`/`DT_STRSZ`/`DT_SYMTAB`/`DT_SYMENT`,
`DT_HASH` and `DT_GNU_HASH`, `DT_RELA`/`DT_RELASZ`/`DT_RELAENT` (and `DT_REL*`),
`DT_JMPREL`/`DT_PLTRELSZ`/`DT_PLTREL`, `DT_SONAME`, `DT_RPATH`/`DT_RUNPATH`, `DT_INIT`/
`DT_INIT_ARRAY`, `DT_SYMBOLIC`/`DT_FLAGS`/`DT_FLAGS_1` (`DF_1_NOW`).

## Appendix B — bookkeeping

- `README.md` "Project Structure": add `crates/ldso`.
- `PORTING_PLAN.md`: a "Dynamic linking" phase entry pointing at this file.
- `Justfile`: `dynlink-x86` / `test-dynlink-x86` (**done**).
- `.agents/skills/minix-boot-process`: a note that a dynamic exec enters `ld.so` first —
  a future boot-chain reader will otherwise conclude the wrong process started.
- Any `.rules` addition (per the hygiene policy, in the PR description, not inline). Two
  candidates from Phase 0: "the exec path has no `e_type` check while `parse_elf_header`
  does — don't conflate them", and "a new directory in `build_minixfs` renumbers every
  inode after it, which the boot test's `/devices` inode 7 depends on".
