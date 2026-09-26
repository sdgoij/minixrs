# minixrs

A Rust port of [MINIX 3.3.0](https://www.minix3.org/), written from scratch.

This project implements the full MINIX 3 stack in Rust — kernel, architecture-specific code, device drivers, filesystem servers, networking, system servers, and userland programs — targeting **x86_64**, **RISC-V64**, **AArch64**, and **wasm32**.

> **▶ [Try it in a browser](https://sdgoij.github.io/minixrs/)** — the whole system as WebAssembly
> instances in a tab. The real kernel, the real servers and the real userland, booting to a `#`
> prompt with a disk, a display, a keyboard and a network. No install, no server.

## Status

Boots multi-process userspace in QEMU on x86_64, RISC-V64, and AArch64 with a serial shell.
The same system runs in a browser tab on wasm32 — the [demo](https://sdgoij.github.io/minixrs/) is
that build, and CI builds and boots the same artifacts on every push.
VFS mounts the root filesystem, MFS reads and writes files, and the shell supports
`>` redirection (create/truncate) for builtin commands **and external binaries**
(exec'd commands write through VFS via a dup2'd fd, so `/bin/echo x > file` works).
Alongside `>` the builtin shell handles `|` pipelines (up to eight stages), `&&`
chaining and trailing `&` background jobs. It is deliberately kept small: GNU bash is
the full shell (built by the fork's stage1 and injected as `/bin/bash`, see below), so
the root shell does not need to grow into one.

See `.agents/skills/` for domain-specific documentation and
[PORTING_PLAN.md](PORTING_PLAN.md) for the task tracker.

> **⚠️ Research project — not production-ready.**  
> If you're looking for a production operating system, use Linux, a BSD, or [Redox](https://www.redox-os.org/) instead.

## Recent work

The last few days moved the project from "boots a shell" to "a real toolchain target":

- **The whole system in a browser tab** — a **wasm32** target where each process is a WebAssembly instance and the host *is* the devices (console, clock, disk, display, keyboard, pointer, the network). The kernel, the servers and the userland are the ones the arches run: the shell forks, execs from the image, writes a file to the disk and reads it back, and `ping 10.0.2.2` reaches a peer over the host's link. [Try it](https://sdgoij.github.io/minixrs/).
- **1:1 kernel threads** — every thread is a schedulable Proc slot: `thread_create`/`exit`/`join`/`yield`, wake-one IPC delivery, group sweep on exit/exec/fork, per-thread TLS. MINIX proper had no native threads; this port does.
- **A working `std` port** — the forked rustc's std PAL for minix (`sys/pal/minix`) runs on the OS: `/bin/hello` is a std-linked binary that spawns threads with TLS and exits cleanly.
- **Networking that works** — virtio-net plus DNS: `/bin/udp nos.nl` resolves hostnames from inside QEMU.
- **Memory from 72M to 16G** — the same kernel boots in ~72 MiB of guest RAM and runs `/bin/hello` up to 16 GiB, on all three hardware arches (x86_64, RISC-V64, AArch64).
- **A heap that actually grows** — userland heap growth routed through VM's brk (demand-mapped, freed on exit); the COW refcount bug that killed repeated `hello` runs is fixed.
- **Honest memory reporting** — the boot banner prints detected vs usable RAM (a 4 GiB guest says `4095 MiB detected (4078 MiB usable)`, not the old "5120 MiB" artifact).
- **uutils/coreutils runs on the OS** — the multicall binary (60 applets) boots, allocates and writes output a second tool reads back; it is embedded on x86_64 and riscv64, gated by `just test-coreutils-wedge`. The coreutils submodule tracks the port (see [coreutils](#coreutils) below).
- **GNU bash runs** — built by the fork's stage1 (a POSIX host, see `C_BUILD.md`), injected as `/bin/bash` and booted: it prints its banner, runs `-c`, loops, arithmetic and redirects, and gets its own `$PWD`. Getting there filled the C surface it needed (`termios`/`ioctl`, `mknod`, `inet_*`, `scanf`, an environment that survives `exec`) plus two gaps it found in the shell and the libc: a shell that did not remove quotes, and a `getcwd` that returned `ENOSYS`.
- **A `ls` that behaves like one** — sorted, and laid out in columns that fit the terminal (80 columns when the tty cannot say, which a serial console cannot), one name per line when the output is a file or a pipe. A wider directory listing is what found an MFS `getdents` bug: at end-of-directory it returned `OK` with a stale reply payload, so a reader asking until it got 0 was handed the same entries for ever.
- **Dynamic linking** — a Rust loader (`crates/ldso`, installed as `/libexec/ld.so`) that VFS's exec enters as a program's `PT_INTERP`, and every image now carries it together with the port's C library as a shared object (`/lib/libc.so`) and a C program linked against it (`/bin/dynclib`). The loader maps each `DT_NEEDED` at a deterministic base, binds eagerly, follows the dependency graph transitively, applies every object's relocations and runs its initialisers in order, maps one file once (by inode, not by the name it was asked for), and gives a dynamically linked program thread-local storage. A program can also `dlopen` one at run time — which is what a driver lookup needs — and linking dynamically stays opt-in as MINIX's is: the default is still static, and a program of your own is `just cdyn`, which links against the `/lib/libc.so` every image already carries; see [below](#dynamic-linking).

## coreutils

`/bin/coreutils` is the port's real toolchain: one multicall binary built from the
[`uutils/coreutils`](https://github.com/uutils/coreutils) fork in `coreutils/`, whose **60
applets** are the subset named by the `feat_minix` feature set in `coreutils/Cargo.toml`. It is
embedded in the image on **x86_64** and **riscv64**. **aarch64** builds it
(`just coreutils-aarch64`, which needs the port's own `cc` for blake3's NEON path) but the image
does not carry it yet, because an applet that allocates heavily — `coreutils seq 200` —
intermittently writes nothing there (KNOWN_ISSUES aarch64 #9). wasm32 does not build it at all:
the multicall links the std PAL, which the browser target does not.

The applets run as `coreutils <name> [args]`; the multicall also dispatches on its own
`argv[0]`, so a link named for an applet works too. The OS has no kernel entropy source, so
`coreutils/src/bin/coreutils.rs` registers a weak SplitMix64 RNG for the applets that want one
(`factor`, `seq`, `shuf`, `sort`) until it grows one. The link is stripped and `opt-level=z` to
keep the ~6 MiB binary inside the default 16 MiB minixfs.

`just test-coreutils-wedge` is the acceptance gate — the scenario in
`tools/smoke/coreutils-wedge.tsv` has `coreutils seq 3` write three lines and the next tool read
them back, the reproducer for the wedge that once left a child dead at its ELF entry
(KNOWN_ISSUES item 12). It runs in QEMU on x86_64, and the same scenario has also been run green
on riscv64.

The excluded applets are the ones the port cannot run yet: `chmod` and `touch` need
`std::os::unix` mode bits and `filetime`, `rm` pulls in `console` with no non-`unix` fallback,
and `pwd` and `mktemp` need a working `std::env::current_dir` (KNOWN_ISSUES item 24). The OS
ships its own `/bin/chmod`, `/bin/rm` and friends for those.

## Dynamic linking

Opt-in, and the default is unchanged: every program the boot path runs is static and non-PIE,
exactly as in MINIX. What an image *carries* is the capability — the loader
`/libexec/ld.so`, the port's C library as a shared object `/lib/libc.so`, and `/bin/dynclib`,
a C program linked against it — so `just build` embeds all three in both images and the boot
test checks for them.

The loader is `crates/ldso`, entered by VFS's exec as a program's `PT_INTERP` interpreter. It
maps every `DT_NEEDED` object at a base from a deterministic allocator (there is no ASLR, so a
loader failure is reproducible), applies each object's `RELATIVE` fixups, resolves `GLOB_DAT`,
`JUMP_SLOT` and `COPY` **eagerly** — an unresolved symbol fails the load rather than faulting
on the first call — follows the dependency graph depth-first, runs the objects' initialisers
in dependency order, and lays out every module of thread-local storage against the thread
pointer, so a library that keeps `__thread` state — and a second one loaded later — each get
their own storage. One
file is one mapping however it is named: the check is the file (`st_dev`/`st_ino`), not the
spelling, so a soname and a path to the same object do not become two copies. The crate is
new rather than a port of `ld.elf_so`, but the rules are the reference's — `search.c`'s "a name
containing a slash is a path", and `load.c`'s path-then-inode "already loaded".

A program can also `dlopen` a shared object at run time and reach its symbols with `dlsym` —
the loader answers the family for `libc.so`, so a C program links nothing extra for it — and
`RTLD_LOCAL`/`RTLD_GLOBAL` scope a load, with `dlclose` accepted and nothing unloaded. What it
does not do yet, each a deferral rather than a stub: unload an object, `LD_LIBRARY_PATH`,
auxv (the loader uses a fixed 4 KiB page), and MINIX's soname/versioned-symlink scheme, so the
search is literal — `/lib/`, `/usr/lib/`, or the name itself when it contains a `/`. Two
*measured* limits bound what can be loaded: a program can carry **18** shared objects, and
**8** of them can bring thread-local storage (`TLS_SLOTS`), the ones it starts with plus a
surplus of 2 KiB slices for later ones — an object past either is refused with a message that
names it. A dynamically linked program is bounded in address space rather than memory: an
address space holds 64 regions, the two images plus the stack and the heap take 8, and an
object costs one region per `PT_LOAD` — three, now that the `GNU_RELRO` segment nothing reads
is not linked.

Building anything dynamic goes through the fork's `-elf` (position-independent) targets, which
`just bootstrap` builds. `tools/build-dynlibc.py` builds `libc.so` and `dynclib` with them
(`just dynlib-<arch>`, which every image build runs). `just cdyn tools/myprog.c` links a
program of your own the way that script links `dynclib` — `-Bdynamic`, `-l:libc.so`,
`--allow-shlib-undefined` for the TLS symbols the loader supplies, and
`--dynamic-linker=/libexec/ld.so` — writes `target/dync/<arch>/<stem>`, and prints the two
commands that build an image with it and boot it (`MINIXFS_EXTRA`; on Windows the hint also
names `MSYS2_ENV_CONV_EXCL`, without which MSYS rewrites the `/bin/...` value on the way to a
native tool and the build refuses it). Every image already carries what the program needs, so
nothing is added to the image but the program itself.

- [DYNAMIC_LINKING.md](DYNAMIC_LINKING.md) — the design decisions, the phased plan (0–7), the
  measurements and the traps; it builds on the file-backed exec path in
  [FILEMMAP.md](FILEMMAP.md).
- `just test-dynlink-x86` (also `riscv64`, `aarch64`) — boots an image and drives
  `tools/smoke/dyn.tsv`: the loader's bases, relocations, transitive load, cross-object
  resolution and initialiser order, before and after a `fork` and a re-`exec`, then the shipped
  `/bin/dynclib`'s line — `errno` read through the loader's thread-local storage, `strerror`'s
  buffer inside `libc.so`, and the program's own `.init_array` — and five `dlopen` steps: the two
  scopes, `dlclose`, and the two failures the loader names itself.
- `just test-cdyn-x86` — the gate on the recipe for a program of your own: it builds
  `tools/cdyn-demo.c` with `just cdyn`, injects the result and boots `tools/smoke/cdyn.tsv`,
  whose line is a string only `libc.so` holds.
- `just probe-dso-share-x86` (also `riscv64`) — measures from the host's page tables that two
  processes mapping one object share its read-only frames (13/13 on x86_64).
- `cargo test -p ldso` — the host side: the ELF parse, every relocation rule, the base
  allocator and the name resolution.

## Quick Start

### Prerequisites

- Rust toolchain (MSRV: **1.96**, edition: **2024**) — the OS is built with
  the forked Rust compiler in the `rust/` submodule; `just bootstrap` builds
  its stage1 compiler + the minix std sysroots (first run needs network)
- bash on PATH (git-bash on Windows) — the Justfile recipes are POSIX sh
- QEMU 11 or newer (`qemu-system-x86_64`, `qemu-system-riscv64`,
  `qemu-system-aarch64`) — the test recipes refuse an older emulator, which hangs
  the aarch64 boot suite
- Clang 22 (x86 trampoline, C smoke tests, the dynamic-linking artifacts on every arch, C++ runtime cross build)
- CMake + Ninja (for the C++ runtime cross build — `just libcxx-x86`)
- [Just](https://just.systems/) (build runner)
- **For the wasm32 target only:** a Rust **nightly** with the `rust-src` component — that target is a
  JSON spec built with `-Z build-std`, so it needs sources rather than a prebuilt sysroot —
  [Binaryen](https://github.com/WebAssembly/binaryen) installed where the build looks for `wasm-opt`
  (`npm install binaryen --prefix tools/fork-spike/.tools --no-save`), and **Node 22.4+** to run the
  check harnesses — one of them drives the WebSocket client Node only ships from 22.4 on. The
  three hardware arches need none of it.

> **Windows users:** Just executes recipes with `sh` (the POSIX shell) —
> without it, Just falls back to `cmd` and the recipes break. Git for
> Windows ships one at `C:\Program Files\Git\usr\bin\sh.exe`; add that
> directory to your `PATH` (or `C:\Program Files\Git\bin`). See
> <https://github.com/casey/just#windows> for how Just selects its shell.

### Usage

```bash
# One-time setup: fetch the rust fork submodule, build its stage1 compiler
# + std for all minix targets, and the /bin/hello std smoke test
just bootstrap

# x86_64
just build                    # Build the kernel + boot images
just run                      # Build and boot in QEMU
just debug                    # Build and boot with GDB server on :1234
just test-qemu                # Run the QEMU integration tests

# RISC-V64
just build riscv64            # Build the RISC-V kernel
just run riscv64              # Boot in QEMU (uses OpenSBI)
just test-qemu riscv64        # Run the QEMU integration tests
just test-boot riscv64        # Run the boot tests

# AArch64
just build aarch64            # Build the AArch64 kernel
just run aarch64              # Boot in QEMU (virt machine)
just debug aarch64            # Build and boot with GDB server on :1234
just test-qemu aarch64        # Run the QEMU integration tests
just test-boot aarch64        # Run the boot tests

# wasm32 — the same system in a browser tab
sh tools/wasm-browser/build.sh    # Build the artifacts and stage the page's copies of them
node tools/wasm-browser/serve.js  # Serve the page, then open http://127.0.0.1:8080/
node tools/wasm-servers/boot.cjs  # Checks: the boot chain and every device, at quiescence
just publish-wasm                 # Or: build and stage the demo into docs/ (what Pages serves)
```

The Just recipes orchestrate plain `cargo` invocations; the initramfs CPIO
and MinixFS root image are assembled by `crates/kernel/build.rs` from the
built userland/server binaries, and the x86 trampoline/kernel.bin post-link
is handled by `tools/mkboot.rs`. Assembled images are mirrored per-target
under `target/images/<triple>/` for host inspection.

`just image [arch]` builds one self-contained, bootable ELF per arch — the
initramfs and root image are embedded, so QEMU needs no disk attached. Prebuilt
ones are attached to releases: a `v*` tag for a release, `image-<sha>`
(prereleases) for a per-commit build. The assets carry the version
(`minix-x86-v0.1.0.elf`), every image in a release was booted by the build that
published it, and the release notes carry the exact `qemu-system-*` command line.

## Project Structure

```
crates/
├── kernel              # Core kernel: processes, scheduling, IPC, VM
├── kernel-boot         # Boot loader & entry point (x86_64 trampoline)
├── boot-image          # Initramfs CPIO + MinixFS image builders (host)
├── arch-common         # Architecture-independent kernel types & ABI
├── arch-aarch64        # AArch64-specific kernel code
├── arch-x86_64         # x86_64-specific kernel code
├── arch-riscv64        # RISC-V64-specific kernel code
├── arch-wasm32         # wasm32 HAL: the kernel with host imports as its devices
├── drivers             # Device drivers (serial, keyboard, etc.)
├── fs                  # Filesystem servers (minixfs, ramdisk, etc.)
├── net                 # Networking stack
├── servers             # System servers (PM, VFS, VM, sched, TTY, etc.)
├── userland            # Userland binaries (cat, ls, sh, etc.)
├── minix-rt            # Userspace runtime: _start, panic handler, syscalls
├── minix-std           # MINIX syscall layer: IPC, endpoints, grants
├── minix-libc          # Minimal libc for FFI
├── libs                # libc, libm, libutil re-implementation
├── ldso                # Dynamic loader (/libexec/ld.so): maps and relocates shared objects
└── minix-util          # Shared utilities
```

Plus the wasm32 target's own crates, which are separate from the workspace because a wasm module has
no host entry point to link:

```
crates/
├── kernel-wasm         # The kernel as a wasm module: the boundary the host instantiates
├── wasm-servers        # The servers as one wasm module, one exported entry point per server
├── wasm-program        # A userland program as its own module — what `exec` instantiates
└── wasm-procs          # Hand-written wasm processes from the M2 dispatch-protocol spike
```

## Architecture

MINIX 3's microkernel design is preserved:

- **Kernel** — process table, scheduling, IPC, virtual memory
- **System servers** — separate user-space processes (PM, VFS, VM, sched, TTY, DS, RS, MFS, ramdisk)
- **Drivers** — hardware abstraction, registered with the kernel
- **Filesystem servers** — minixfs, ramdisk, etc.
- **VFS** — virtual filesystem layer for unified file operations
- **Userland** — classic POSIX utilities (cat, ls, cp, rm, sh, etc.)

The project supports **x86_64**, **RISC-V64**, **AArch64**, and **wasm32** targets via architecture-specific crates (`arch-x86_64`, `arch-riscv64`, `arch-aarch64`, `arch-wasm32`) sharing a common core (`arch-common`).

On wasm32 the mapping is unusual enough to be worth stating: a *process* is a WebAssembly instance
and an address space is its linear memory, so the "architecture" is the host. Every hardware
boundary the other three arches implement in assembly or device registers — context switching,
interrupts, paging, port I/O, PCI — becomes a host import or a dispatch decision, which is what
`ARCH_WASM32.md` is about. The kernel, the servers and the system calls above that boundary are the
same code on all four.

See `.agents/skills/` for domain deep-dives:
- `minix-boot-process` — boot chain from QEMU to shell
- `minix-ipc-patterns` — message formats, SENDREC semantics, grants
- `minix-server-patterns` — main loop, dispatch, SEF callbacks
- `minix-c-to-rust` — struct layout, type mapping, no-stubs policy

## Testing

- **Host tests:** `cargo test` — pure-logic unit and property tests
- **QEMU integration:** `just test-qemu [arch]` — kernel tests running in QEMU
  (page tables, IPC, scheduler, timers, syscalls, ELF loading, grants):
  - `just test-qemu` (x86_64) — 91 tests, exits with a real pass/fail code
  - `just test-qemu riscv64` — 76 tests, paging enabled
  - `just test-qemu aarch64` — 76 tests, MMU enabled

  RISC-V/AArch64 integration builds enable the MMU before running the shared
  suite, so copy_from_user / delivermsg perform real page-table walks, the
  SENDREC payload assertions run on all three hardware arches, and per-arch
  hardware tests probe the actual devices: RISC-V CLINT timer (rdtime + SSTC
  stimecmp) and SBI console; AArch64 generic timer (cntpct_el0/cntfrq_el0)
  and PL011 UART.
- **Boot tests:** `just test-boot [arch]` — multi-server verification after VFS
  mount_root on all three hardware arches (server liveness, process-table
  consistency, VFS→MFS readsuper IPC round-trip, brk/RAM-disk mappings,
  allocator, initramfs), then a userspace phase in which a test init execs a
  program from the image and the kernel requires the exec'd entry page to be
  mapped user+executable
- **One scenario, four targets:** `tools/smoke/scenario.tsv` is the userspace smoke
  test — a program exec'd from the image, a file written, that file read back — and all
  four targets read that one file. `just image [arch]` builds the shipped ELF and types
  the steps into the shell that comes up, through `tools/smoke/feed.sh`; `run.js` drives
  the same steps in the wasm engine. `arch-tests` runs it on every PR, so an image whose
  shell cannot run a command has to fail the build rather than a download.
- **Dynamic linking:** `just test-dynlink-{x86,riscv64,aarch64}` boots an image and drives
  `tools/smoke/dyn.tsv` — the loader's relocations, transitive load, cross-object resolution
  and initialiser order, across a `fork` and a re-`exec`, then the shipped `/bin/dynclib`
  against `/lib/libc.so`. It is a per-arch scenario rather than another `scenario.tsv` step
  because wasm drives that one and has no dynamic linking. `just test-cdyn-x86` is the gate on
  `just cdyn`, the recipe for a program of your own: it builds `tools/cdyn-demo.c` with the
  recipe, injects it and boots `tools/smoke/cdyn.tsv`. `just probe-dso-share-{x86,riscv64}`
  measures read-only frame sharing between two processes from the host's page tables, and
  `cargo test -p ldso` covers the loader's parsing, relocation rules and name resolution on
  the host. The boot test asserts that an image carries the loader and the two objects.
- **wasm (the browser target):** `sh tools/wasm-browser/build.sh` builds and stages the artifacts
  first (it runs `tools/wasm-servers/build.sh`, then copies the three into the page's `build/`), then
  - `node tools/wasm-servers/boot.cjs` — the boot chain and every device, at quiescence (82 checks)
  - `node tools/wasm-browser/run.js` — the same artifacts at a *live* prompt (22)
  - `node tools/wasm-browser/page.test.js` — the page's own code under a stub DOM (46)
  - `node tools/wasm-browser/net.test.js` and `node tools/wasm-net/relay.test.js` — the network
    link's frames, and the relay over a real socket (27 + 10; these two need no artifacts at all)

  All of it runs in CI (`wasm-tests`, `wasm-wire-tests`), which matters more on this target than on
  the others: wasm has no page faults, so the file-backed exec path is absent there, and what the
  harnesses *can* see of the same code is the only signal there is.

RISC-V and AArch64 exit QEMU via SBI reset / PSCI (no exit-code device on
this QEMU build), so their recipes pass/fail on the serial log markers.

See `minix-testing` skill in `.agents/skills/` for full patterns and isolation mechanisms.

The original C reference source is at `.refs/minix-3.3.0/` (git submodule).

## Build & Development

- **Build runner:** `Justfile` — run `just <recipe>` for available commands
- **The browser demo:** [sdgoij.github.io/minixrs](https://sdgoij.github.io/minixrs/) — `just publish-wasm`
  builds the wasm system (kernel, servers, image) and stages the page into `docs/`, which GitHub Pages
  serves straight out of the repository: no CI, no server, no account. The assets are committed, so a
  published demo costs about 19 MiB per build; `tools/wasm-browser/README.md` has the settings, and
  the staging ends by booting the staged copy, so what gets served is what was tested.
- **Cargo features:**
  - `embed_initramfs` — embed initramfs in the kernel binary
  - `embed_minixfs` — embed minixfs driver in the kernel
  - `qemu-tests` — enable QEMU integration test infrastructure
- All three hardware arches build with the fork's stage1 compiler and in-tree minix
  targets (`x86_64-pc-minix`, `riscv64gc-unknown-minix`, `aarch64-unknown-minix`)
  — no `-Zbuild-std`, no JSON specs. `just bootstrap` builds the compiler once, and
  only wasm32 is built with the nightly described above.

## License

Licensed under the [GNU General Public License v2.0](LICENSE.md).

MINIX 3 source code references are used under the [LICENSE.MINIX](LICENSE.MINIX).
