# A development toolchain inside minixrs

Status: **Phase 0 landed** (`libc.a` and a clang+LLD link with no rustc, gated on
all three arches), **Phase 1 landed** (`just llvm-host-tools` builds LLVM 23's
`llvm-tblgen`, `clang-tblgen` and `llvm-min-tblgen` for the host), and **Phase 2 is
built and running**: `clang` 23 and `ld.lld` are minix ELFs, `clang --version` and
`clang -c` work in the guest, and the in-guest LLD's output currently hangs
(`KNOWN_ISSUES.md` item 83). Phases 3 to 5 are the design.

Related: [`c_build.md`](c_build.md) (the host rule this has to extend),
[`dynamic_linking.md`](dynamic_linking.md) (the DSO and loader the in-guest
linker will target), [`memory_budget.md`](memory_budget.md) (the guest's RAM),
`.agents/skills/mesa-port` (the libc++ runtime this reuses) and
`.agents/skills/minix-boot-process`.

## 1. Goal, and what "done" means

A compiler that runs **inside** a minix guest and produces programs for that
guest: `clang`/`clang++` for C and C++, and `rustc` for Rust. Not a
cross-compiler on the host. Concretely, on x86_64 first and then the other two
arches:

- `/usr/bin/clang --version` answers in the guest.
- `/usr/bin/clang -c hello.c` produces an object in the guest, and
  `/usr/bin/ld.lld` links it against the guest's `libc.a` into a program that
  runs, all with no host involved after boot.
- `/usr/bin/rustc hello.rs` does the same for Rust.
- The toolchain can rebuild itself: the guest's clang compiles LLVM, and the
  guest's rustc is what `x.py` uses as `stage0` for a minix host.

Non-goals for the first pass: every LLVM target (the guest needs its own, the
cross target it will compile for, and nothing else), shared LLVM (`libLLVM.so`),
tests, docs, sanitizers, and a package manager.

## 2. What the tree already has

The hard half of a C toolchain exists; what is missing is the compiler and the
linker as guest programs.

- **A real C library with a C ABI.** `minix-libc` exports the POSIX surface
  (`stdio`, `stdlib`, `string`, `wchar`, `locale`, `time`, sockets, termios,
  pthreads over kernel threads, `mmap`, `dlopen`) over `minix-std`. `tools/c-include`
  is the matching header set and `cargo xtask check headers` keeps the two sides
  in step.
- **The runtime as an archive a foreign linker understands.** `cargo xtask build
  libc-static <arch>` builds `libminix_libc.a` with
  `--crate-type staticlib --features staticlib` and copies it to
  `target/<triple>/release/libc.a`. The `staticlib` feature supplies only the
  `panic` lang item a final artifact needs; the static TLS path is unchanged.
- **A C++ runtime.** `just libcxx <arch>` builds libc++ and libc++abi into
  `target/cxx/<arch>/minix-runtime/libstdc++.a` (no exceptions, no RTTI, no
  localization, no `<filesystem>`). Mesa is already built and shipped on it.
- **The start-up and layout.** `tools/crt0-<arch>.S` takes the SysV initial
  stack, initialises TLS, runs `.init_array` and calls `main`;
  `tools/minix-user.ld` page-aligns every section and carries the TLS bounds. The
  per-arch base (`0x4000000` on x86_64, `0x1000000` elsewhere) and the loader
  base now live in `crates/xtask/src/arch.rs`, mirroring the fork target spec's
  `pre_link_args`.
- **A linker that runs where the compiler does.** `toolchain::find_lld` resolves
  the fork's `rust-lld`/`lld`, and LLD's GNU flavor links the port's ELFs. The
  guest will need a copy of the same LLD as a program.
- **A disk-root boot.** The embedded root is a 16 MiB ramdisk window
  (`RAMDISK_IMAGE_SIZE`); the disk path (`just run-x86`, virtio-blk +
  `target/images/<triple>/disk.img`) has no such limit because MinixFS V3 uses
  4 KiB blocks and 32-bit zone numbers.
- **The fork's LLVM sources, and one host tablegen.** `rust/src/llvm-project` is
  LLVM 23.1.1. `rust/build/x86_64-pc-windows-msvc/llvm/bin/llvm-tblgen.exe` is a
  matching native tool; `clang-tblgen` and `llvm-min-tblgen` are **not** there.
- **Phase 0 is gated.** `just test-cdirect-<arch>` compiles `tools/hello.c` with
  clang, links it with LLD against `libc.a`, `crt0` and `minix-user.ld` (no
  rustc in the line), boots it as `/bin/cdirect` and asserts its line. Passes on
  x86_64, riscv64 and aarch64.

## 3. Why the two toolchains are separate problems

**Clang is a one-shot cross-build, then self-hosting.** LLVM is C++ and its
CMake build is self-contained: host tablegen programs can be supplied
(`LLVM_NATIVE_TOOL_DIR`), the target compilers are the host's clang, and the
final link is a program's link, not the host's. Once `clang` runs in the guest,
a second build in the guest is evidence (and the reproduction the goal asks
for), not the only route.

**rustc is a bootstrap.** `x.py` runs `stage0` on the build host, so
`--host x86_64-pc-minix` needs a minix rustc before x.py can start. The
pragmatic route is a one-off cross-build of `rustc_driver` + `rustc` for the
minix target with the existing stage1 (a `cargo build`, not a bootstrap), then
wiring that binary in as the stage0 for `--host x86_64-pc-minix`. That binary
also has to run in the guest, which is why it comes after clang: the same libc
and linker surface must be known good first.

## 4. Phases

### Phase 0: the C link without rustc (done)

`cargo xtask build libc-static` and `cargo xtask build c-direct`, plus
`just test-cdirect-<arch>`. Two things this settled:

- A `staticlib` of `minix-libc` is a sound `libc.a`: `core` and
  `compiler_builtins` travel in it, and LLD pulls only the members it needs.
- LLD's GNU flavor links the result with the same script and bases the rustc
  link uses, so a non-Rust driver can produce a working program.

### Phase 1: host tablegen for LLVM 23 (done)

A cross build of clang needs `clang-tblgen` for the build host, and LLVM 23's
build also asks for `llvm-min-tblgen`; only `llvm-tblgen` exists in the fork's
build tree. `just llvm-host-tools` (`cargo xtask build llvm-host-tools`)
configures a native build of `rust/src/llvm-project/llvm` with clang enabled and
builds all three into `target/llvm/host/Release/bin` (MSVC and the Visual Studio
generator on Windows, Ninja elsewhere). The tools report `23.1.1`, which is the
source in the tree.

A tablegen from an older LLVM is not interchangeable: the `.td` files use records
and features the older tool does not know, so the system's LLVM 22 tablegen is
not a substitute. Point a cross build at the directory with
`-DLLVM_NATIVE_TOOL_DIR=target/llvm/host/Release/bin`.

### Phase 2: cross-build clang and lld for minix

One CMake configure per arch, against `rust/src/llvm-project/llvm`, with
`-DLLVM_ENABLE_PROJECTS="clang;lld"` and:

- `LLVM_TARGETS_TO_BUILD` = the guest's own target plus the one it will compile
  for (x86_64 first, so `X86`).
- `LLVM_NATIVE_TOOL_DIR` = the directory `just llvm-host-tools` printed
  (`target/llvm/host/Release/bin` on Windows, `target/llvm/host/bin` elsewhere),
  `CMAKE_CROSSCOMPILING=ON`, `CMAKE_SYSTEM_NAME=Generic`,
  `CMAKE_SYSTEM_PROCESSOR=<machine>`, and the existing
  `target/cxx/toolchain-*.cmake` flags (`-nostdinc`, the clang resource dir,
  `tools/c-include`, libc++ headers, `-fno-stack-protector`, `-fPIC`).
- `LLVM_DEFAULT_TARGET_TRIPLE` = the arch's LLVM triple (`x86_64-unknown-none`),
  so the guest's clang needs no `--target` for its own programs.
- `LLVM_ENABLE_THREADS=ON` (the port's heap is futex-locked and `std` threads work:
  `/usr/bin/threadprobe` is the gate; `KNOWN_ISSUES` item 91 records that lld's own
  thread pool still deadlocks, so its links are pinned to `--threads=1`),
  `LLVM_ENABLE_EH=OFF`, `LLVM_ENABLE_RTTI=OFF`, `LLVM_ENABLE_ZLIB=OFF`,
  `LLVM_ENABLE_ZSTD=OFF`, `LLVM_ENABLE_TERMINFO=OFF`,
  `LLVM_ENABLE_LIBXML2=OFF`, `LLVM_ENABLE_LIBEDIT=OFF`, `BUILD_SHARED_LIBS=OFF`,
  `LLVM_BUILD_LLVM_DYLIB=OFF`, `LLVM_INCLUDE_TESTS=OFF`,
  `CLANG_ENABLE_ARCMT=OFF`, `CLANG_ENABLE_STATIC_ANALYZER=OFF`,
  `LLVM_INCLUDE_BENCHMARKS=OFF`.
- A **link driver** that is not clang's own: CMake's `Generic_GCC` toolchain
  falls back to `gcc` for this triple on Windows, and on either host it would
  search for a libc it must not find. `CMAKE_CXX_LINK_EXECUTABLE` is overridden
  to invoke LLD with `crt0.o`, `libc.a`, `libstdc++.a`, `-T tools/minix-user.ld`
  and the arch bases, which is exactly the Phase 0 line.
- `CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY` for the configure probes (they
  must not need a runnable guest), with `CMAKE_C_COMPILER_WORKS=1` and
  `CMAKE_CXX_COMPILER_WORKS=1`.

Output: `target/toolchain/<arch>/bin/clang`, `clang++`, `ld.lld` (stripped), and
the C++/C headers the compiler searches. The gate is `clang --version` **on the
host** first (the ELF's `e_machine` and entry are checked), then in the guest in
Phase 3.

Known unknowns this phase exists to discover: which `tools/c-include` entries
LLVM's `Support`/`ADT` need and the port does not provide; whether libc++'s
`<filesystem>`-off configuration is enough (LLVM's `Support` has its own
filesystem layer); and whether `Execute`/`Process` need `posix_spawn`, which the
port does not export (LLVM falls back to `fork`+`exec` when the configure test
says so, and those the port has).

#### Phase 2 progress: what compiled, and the one ABI gap

The cross configure is clean on the Windows host with MSVC and the fork's stage1.
`LLVMSupport`, the X86 backend and the whole of `lld` compile, and `bin/lld`
links to a 67 MiB minix ELF at `0x4000000`. The gaps the build found, and the
fixes that landed, are all in `tools/c-include` and `minix-libc`:

- the full POSIX errno set (`errc.h` maps `std::errc` onto it unguarded);
- `fegetexceptflag`/`fesetexceptflag`, `FP_ILOGB0`/`FP_ILOGBNAN`, `erf`/`erfc`;
- `sigismember`'s `si_pid`/`si_uid`/`si_status` member aliases;
- `langinfo.h` and `nl_langinfo_l(CODESET)`, which libc++'s Linux-less locale
  fallback calls;
- `__cxa_thread_atexit`, with per-thread key destructors now run by the thread
  trampoline, `pthread_exit` and `exit`;
- a `PROVIDE(main = _Z4mainiPPc)` in `tools/minix-user.ld`: a freestanding C++
  target mangles `main`, and `crt0` calls the C name.

The link itself needs a driver, because CMake's rule hands the line to clang,
which on Windows falls back to Strawberry GCC for this triple. `cargo xtask
minix-ld` reads the response file and translates `-Wl,...`, `-lpthreads` and
`-static` into the Phase 0 LLD line; `--no-dependent-libraries` is what makes
libc++'s `.deplibs` names (`pthread`, `rt`) harmless.

**The one remaining blocker is `struct stat`.** libc++'s `<fstream>` is entirely
inside `#if _LIBCPP_HAS_FILESYSTEM && _LIBCPP_HAS_LOCALIZATION`, and
`std::filesystem` reads `st_atim`/`st_mtim` (a `timespec`, for the nanosecond
fields). The port's `struct stat` carried only `st_atime`/`st_mtime`/`st_ctime`
as `time_t`, matching the 88-byte VFS `Stat`. That gap is now closed: the C
`struct stat` has the POSIX shape (three `struct timespec`s with `st_atime` etc.
as member macros; the first 64 bytes keep their offsets, the struct grows from 88
to 112 bytes), and the libc `stat` wrappers fill `tv_nsec = 0` because the VFS
keeps seconds. `fseeko`/`ftello`, `truncate`, `utimensat`, `pathconf`,
`setbuf`, `O_DIRECTORY`, `O_NOFOLLOW` and `AT_REMOVEDIR` came with it.

With that, the whole of clang and LLD builds and links. The toolchain root is
staged under `target/toolchain/x86/root` (clang 87 MiB and ld.lld 54 MiB stripped,
the C and C++ headers, clang's resource headers, `libc.a`, `libstdc++.a`,
`crt0.o`, `minix-user.ld`). `crates/boot-image` now reads `MINIXFS_ROOT` (a whole
tree, not the one-file `MINIXFS_EXTRA`) and creates nested directories, and
`MINIXFS_INODES` overrides the default 512 inode table; the 288 MiB image holds
2142 files. In the guest, `clang --version` answers `clang version 23.1.1` and
`clang -c hello.c` writes `/tmp/hello.o`.

The defect, found and fixed: the in-guest LLD's output had the same size, ELF header and
program headers as the host LLD's, but only the first 19 pages and the last page of the file
held the linker's bytes. It was not the linker. VM's writeback of a writable `MAP_SHARED`
region scans the file-page cache, and an entry that cannot be admitted to the 4096-page
(16 MiB) table is never written back; `ld.lld`'s own 54 MiB image filled it, so the output
mapping's later pages stayed private and the file kept zeros. The cache is now 32768 pages
(128 MiB) and an admission that cannot be satisfied is reported once. The same objects and
link line now produce a byte-identical binary, and it prints `hello from C` in the guest.
`KNOWN_ISSUES.md` item 83 has the measurements and the remaining risk (a working set larger
than the ceiling loses the same way, visibly).

### Phase 3: the toolchain root and a bootable disk image

`cargo xtask build llvm-minix <arch>` is the whole phase: one CMake build per arch
against `rust/src/llvm-project/llvm`, a generated toolchain file, `cargo xtask
minix-ld` as the link rule, and then the staged root. It needs three things that
are built elsewhere and reports each by name when it is missing: the build host's
tablegen (`just llvm-host-tools`), the C++ runtime
(`target/cxx/<machine>-toolchain/minix-runtime/libstdc++.a`), and `libc.a`
(`cargo xtask build libc-static <arch>`). Artifacts: `target/llvm-<arch>/{build,
toolchain.cmake}` and `target/toolchain/<arch>/root`.

A staged root under `target/toolchain/<arch>/root`:

```
/usr/bin/clang, clang++, ld.lld
/usr/libexec/clang  the real driver
/usr/include        tools/c-include
/usr/include/c++/v1 libc++ headers
/usr/lib/clang/<n>  clang's resource headers
/usr/lib/libc.a, libstdc++.a, crt0.o
/usr/lib/minix-user.ld
/usr/src/hello.c
```

Ship it on the **disk** path only (never the embedded ramdisk):
`system-image-x86` includes the staged root with `MINIXFS_ROOT` when it is
there, the same way it includes `/bin/bash`, and skips it with a line when it is
not. `toolchain-root-x86` is the explicit builder (`just llvm-minix x86`); the
toolchain gates depend on it and then build the system image. `just run x86
2048M` boots the same minimal boot image. The root is about 250 MiB, so the
image is 384 MiB (98304 blocks) with 16384 inodes when included. The shell
searches `/bin`, `/sbin` and then `/usr/bin` for a bare name, so `clang`,
`clang++` and `ld.lld` work by name, not only by their full path.

`/usr/bin/clang` is `tools/clang-wrapper.c`, not the driver renamed: it execs
`/usr/libexec/clang` with the sysroot flags, and adds the link inputs (`crt0.o`,
the archives, the script, the image bases) only when the line is not `-c`, `-S`,
`-E` or a query. A link runs as a child so the output can be made executable.
Four things about the *driver* had to be stated explicitly, and each cost a
failed gate to find:

- **`--target=<machine>-unknown-none-elf`.** The `-elf` environment is what
  selects clang's bare-metal toolchain (`isX86BareMetal` wants `UnknownOS`
  *and* environment `elf`). Without it the driver picks `Generic_ELF`, whose
  linker step is a `gcc` this system does not have. BareMetal's linker step is
  `ld.lld` itself.
- **`-no-canonical-prefixes`.** With canonical prefixes the driver calls
  `llvm::sys::fs::getMainExecutable`, and LLVM's Unix implementation has no
  minix branch: the `/proc/self/exe` read is under `__linux__`, which the
  `-none-elf` triple does not define, and its `dladdr` fallback has no answer
  for a static executable. It returns `""`, so the `-cc1` program name is empty
  and every compile fails with `ENOENT`; `-ccc-install-dir` does not fill
  `ClangExecutable` either (measured with `-###`: `"" "-cc1" …`). `argv[0]` is
  the driver and is under the wrapper's control. `/proc/self/exe` and
  `/proc/<pid>/exe` now exist (`KNOWN_ISSUES.md` item 85), so the gap that
  remains is LLVM's probe alone (item 86).
- **`-ccc-install-dir /usr/libexec`** makes `InstalledDir` and the resource
  lookup independent of the above.
- **`-nostdlib -nostartfiles`**, then the driver would otherwise look for a
  `crt1.o` and a `-lc` that do not exist.

`cargo xtask minix-ld` passes `-flavor gnu`: the host `lld` it names is the
generic driver (`lld.exe`), not a flavour-named copy, and it refuses to link
ELF without that first argument.

Gate (`just test-toolchain-x86`, `tools/smoke/toolchain.tsv`): boot the image,
then in the guest `clang --version`, `clang -c /usr/src/hello.c`, `clang
/usr/src/hello.c -o /tmp/hello` and run it. The wrapper's own link line is what
step 3 exercises, and step 4 can only pass if the linked program is a working
one. `hello.c` ships in the image under `/usr/src` rather than being typed.
The gate is not in `test-arches`: the staged root costs an LLVM cross build, so
a missing root is refused by name instead.

### Phase 4: self-hosting clang (FileCheck and llvm-min-tblgen done)

**FileCheck.** The first target is done: the guest compiles LLVM's FileCheck
from `lib/FileCheck/FileCheck.cpp` and `utils/FileCheck/FileCheck.cpp` against
the staged include tree and `libLLVMSupport.a`, links it with the guest's
`ld.lld`, and runs it on a check that must fail. `tools/cxx/fc-*.rsp` keep the
command lines short; `-c @file` and a link `@file` both work. The 22-step
scenario took **24m52s** of guest wall time, from the disk image at 18:48:39 to
the last step at 19:13:32, against the 3600s budget in
`just test-toolchain-x86`. Nearly all of that is the two TUs and the link. The
measurement was at 512M; the gate now names 1024M for headroom.

**llvm-min-tblgen.** The second target is done: the guest compiles every source
of `obj.LLVMTableGenBasic` (thirteen files) plus `llvm-min-tblgen.cpp`, links
them with the guest's `ld.lld` against the cross-built `libLLVMTableGen.a`,
`libLLVMSupport.a` and `libLLVMDemangle.a`, and runs the result on a `.td` that
defines a class and a record. The archives are built `LLVM_ENABLE_RTTI=OFF`, so
the guest units take `-fno-rtti`; the link takes `--gc-sections` and the
Demangle archive, as the cross link does. `tools/tblgen-probe.td` is the input.

`just test-tblgen-x86` is **two boots**. The build boot runs 31 steps and took
**68m06s** at `-m 2048M` (05:50:24 to 06:58:30). The run boot takes **1.8s**
(06:58:30 to 06:58:32) and requires only the binary and the probe.

Memory is the limiting factor, and the numbers are measured rather than guessed.
At 512 MiB `TargetFeaturesEmitter.cpp` thrashed until the 90-minute budget ran
out. At 1 GiB a single `TargetFeaturesEmitter.cpp` completed in 4m19s end to end,
and the full sequence reached the thirteenth unit. At 2 GiB every unit and the
link complete. `memstat` shows the cost: the first compile drops free pages by
40.6 MiB (the file-page cache warming), each later compile by about 2.2 MiB,
and `regions` stays flat at 41 pages / 87.

The second boot is not cosmetic. If the generator runs in the boot that built
it, `-print-records` never returns, although the same binary answers `--version`
there and the host-linked binary runs the same probe on a fresh boot.
`KNOWN_ISSUES.md` item 89 has the samples and the hypothesis (item 83's cache
risk). The gate keeps the build and the run apart for that reason.

Two gaps surfaced and are closed:

- MFS's in-core inode table was sized for a boot workload (512). clang keeps
  every included header's buffer alive for diagnostics, so a real LLVM TU
  exhausted it; `get_free_inode` answered `ENFILE`, which surfaced as a header
  "not found" and then as a failed output-file open. The table is now 2048 with
  a 256-bucket hash, costing ~240 KiB of MFS BSS.
- `tools/c-include/assert.h` declared `__assert_fail` with three arguments and
  no C linkage. libc++'s `<cassert>` includes that header in a C++ TU, so the
  name mangled and the link failed; the signature also did not match
  minix-libc's glibc-shaped four-argument export. The declaration is now
  `extern "C"` and takes `__func__` as its fourth argument.

The next target, if Phase 4 is continued, is the full `llvm-tblgen` (the
`obj.LLVMTableGenCommon` backends) or a full clang. The measurements above are
the budget to plan against, not estimates: 5.6k lines across fourteen files took
68 minutes in TCG, so a full self-host is far beyond one session.

### Phase 5: rustc for minix

Cross-build `rustc_driver` and `rustc` for `x86_64-pc-minix` with the existing
stage1 (the same shape `cargo xtask build dynlibc` uses, but for a host target).
Two pieces are specific:

- `rustc_llvm`'s build script needs a **runnable** `llvm-config` that reports
  the minix LLVM build's libraries and `--cxxflags`. The generated
  `llvm-config` is a minix ELF and cannot run on the build host, so
  `cargo xtask llvm-config --build-root target/llvm-<arch>/build [QUERY...]`
  reads the build's `LibraryDependencies.inc` and `llvm-config.h` and answers
  the same queries. Its `--cxxflags` names the cross build's own clang resource
  headers, which are the version the libraries were built with; the host clang's
  are a different version and can sit under a path with a space, which
  `rustc_llvm` splits because it takes the output unquoted. `cargo xtask build
  llvm-minix` also builds `LLVMMCA` and the arch's `TargetMCA`: `clang` does not
  pull them in, but the arch component's dependency list names them and rustc's
  link needs them.
- `rustc` needs its sysroot (the minix `std`/`core`/`alloc`) staged into the
  guest, which is the same root tree as Phase 3 plus `target/<triple>/release/libstd*.rlib`.

Gate: `/usr/bin/rustc --version` in the guest, `rustc /usr/src/hello.rs`, and
the program runs. Then a bootstrap: `x.py` with that binary as stage0,
`--host x86_64-pc-minix`.

#### Phase 5 progress: the compiler compiles in the guest (2026-11-24)

The gate passes: `/usr/bin/rustc --version`, `threadprobe` (a `std` program whose
worker thread builds a TLS block and walks the sysroot), a full compile and link
of `/usr/src/hello.rs`, and running the result, which prints `hello from Rust`
(`cargo xtask gate rustc --arch x86`). `rustc` is a static ELF built from
`crates/rustc-minix`, its own workspace, so `rust/` carries no scaffolding for
it; the C++ backend is the statically selected `LlvmCodegenBackend`.

What each blocker turned out to be, because three of the five looked like
something else:

- **The compiler hung while reading the sysroot.** `fs_getdents` in MFS had a
  `while` and a `for` where a `break` on a full reply buffer re-read the same
  block forever, so a directory with more entries than one 1024-byte reply holds
  (the sysroot has ~50 rlibs) never finished. `crates/fs/src/mfs/protect.rs`.
- **Four LLVM options were "unknown", including ones the binary clearly
  contains.** The first theory was that the archive members registering them were
  dropped by the static link, so `crates/xtask` extracted `DwarfDebug.cpp.o`,
  `MergeFunctions.cpp.o`, `FunctionImport.cpp.o` and `InlineFunction.cpp.o` and
  linked them again. That was wrong and harmful: the archives already provide all
  four (both copies were visible in `.init_array`), and registering a `cl::opt`
  twice makes LLVM's `CommandLineParser::addOption` call `report_fatal_error`. The
  real cause was that no C++ static constructor ran at all (item 90);
  `crates/rustc-minix/src/main.rs` now walks `.init_array` before the driver
  starts. The extraction is gone.
- **`Result::unwrap()` on `JoinPathsError` at `linker.rs:135`.** std's minix arm
  in `library/std/src/sys/paths/mod.rs` took `split_paths`/`join_paths` from
  `unsupported.rs`, whose `join_paths` always returns `Err`, so setting the
  linker's `PATH` panicked every compile. Both are now implemented in
  `sys/pal/minix/path.rs` with the POSIX `:` separator, next to that PAL's
  `chdir`/`getcwd`/`temp_dir`.
- **Undefined `<*const ()>::is_aligned_to` and `Range<usize>` iterator
  instantiations.** The staged `/usr/lib/libminix-libc.rlib` was the archive from
  the `rustc-minix` build graph (debug profile), while the fixture binaries link
  the release one. `std_hello::libc_rlib` already documents why the two cannot be
  mixed: the debug archive names generics it expects `core` to have codegen'd, and
  the sysroot's `libcore.rlib` exports none of them. `stage()` now stages the same
  release archive the probes use.
- **The link never returned.** lld with its own default thread pool deadlocks;
  `--threads=1` links the identical input in seconds. Pinned in the target spec's
  `post_link_args` (item 91).

Two smaller things: `libc/src/minix/dlfcn.rs` used `#[derive(Clone, Copy)]`
without the crate prelude, which builds under `no_std` but not under the
`#![no_core]` form `rustc-dep-of-std` selects, so `x.py` could not rebuild `std`
until it imported `crate::prelude::*` (that prelude is where `derive` comes from
in `no_core`); and `KNOWN_ISSUES` item 86's `dladdr` note is what makes
`Dl_info` necessary at all.

Next: use the in-guest compiler as stage0 for an `x.py` bootstrap with
`--host x86_64-pc-minix`.

## 5. Constraints and traps

- **Not the embedded root.** `RAMDISK_IMAGE_SIZE` is 16 MiB. A toolchain image
  is the disk path or it is nothing.
- **The image builder's inode table defaults to 512.** `MINIXFS_INODES` raises
  it, and the toolchain image uses 16384. MFS's in-core table is a separate
  number (now 2048) and is what an in-guest compile exhausts.
- **Guest RAM.** `run` defaults to 256M. The toolchain gates name their own
  `-m`: FileCheck uses 1024M, and the TableGen build uses 2048M. 512M is not
  enough for `TargetFeaturesEmitter.cpp` (it thrashed for the whole budget), and
  1G completes single units but not the fourteen-unit sequence. Do not quietly
  raise the default.
- **Host choice.** Phase 1 was built on the Windows host with MSVC 2022: the
  fork's build tree and the stage1 are both here, and MSVC is present. LLVM's
  CMake build is self-contained enough for that (unlike a `configure`/`make`
  third-party tree, see `c_build.md`). The Phase 2 cross build should use the
  same host for the same reason; WSL remains the alternate and already builds
  libc++/Mesa, but its `llvm-tblgen` is 22 and would have to rebuild the host
  tools first.
- **Tablegen version.** Host `llvm-tblgen` must be LLVM 23, the source in
  `rust/src/llvm-project`. The system LLVM 22's tablegen is not a substitute.
- **No exceptions, no RTTI.** LLVM's build options and the libc++ runtime
  already agree on this. A library that assumes otherwise will fail at link,
  not at compile.
- **`posix_spawn` is absent.** If LLVM's configure detects it, the guest's clang
  can spawn the linker. Check the configure answer rather than adding a stub:
  `fork`/`execvp`/`waitpid` exist and are the fallback.
- **`sched_getaffinity` is absent**, which is a feature test to read, not to
  guess: `sysconf(_SC_NPROCESSORS_ONLN)` answers 1 (`crates/minix-libc/src/c_sys.rs`),
  and lld's thread pool deadlocks in-guest anyway, so the port's links are pinned
  to `--threads=1` (`KNOWN_ISSUES.md` item 91).
- **Do not ship a `libLLVM.so`.** Static archives keep the image smaller and the
  loader out of the compiler's start-up.
- **A toolchain boot is not a release artifact.** `just image <arch>` stays the
  self-contained 16 MiB system. The disk system image includes the toolchain
  when its root is present, built through `toolchain-root-<arch>`; `just run
  <arch> 2048M` boots it, and the release artifact does not grow.
- **No debug prints in the kernel or servers.** A bring-up that hangs is a
  `bare-metal-debug` problem (LLDB + QEMU `-s`), not a `serial_write` problem.
- **`cargo xtask build c-direct` is a canary.** If it breaks, the `libc.a`
  interface LLVM's build and the guest's linker depend on has broken, even if
  every rustc-linked binary still works.
