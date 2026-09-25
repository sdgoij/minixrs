# C_BUILD.md — building third-party C for minix, and the one host it needs

## What holds today: any host builds minix

Everything in this repo builds on Windows and on Linux alike, and that property is
worth keeping in view because the next section is where it stops.

- The userland is Rust, built by the fork's stage1 for the in-tree minix targets.
- The two in-tree C programs (`tools/hello.c`, `tools/ctest.c`) are built by
  `tools/build-c-hello.py`: clang for the compile half
  (`--target=<machine>-unknown-none`), the fork's rustc for the link half,
  against `tools/c-include`, the target's `tools/crt0-<arch>.S`,
  `tools/minix-user.ld` and the `minix-libc` rlib.
- All three arches build C now, not just x86_64: `tools/ccarch.py` holds the
  triple and clang machine per target, and what that took is in "A three-arch C
  surface" below.

Neither needs a POSIX host. The compiler runs *on* Windows, the headers are the
port's own rather than the system's, and every path in the link is passed
explicitly.

## What breaks it: a third-party project's build is not self-contained

bash (a source checkout, fetched at a pinned commit by `tools/build-bash.py`)
does not build this way, for reasons that have nothing to do with minix:

- **Its tree lacks generated sources.** A git checkout has no `signames.c`, no
  `syntax.c`, no `builtins/builtext.h`. `make` therefore builds `support/mksignames`,
  `mksyntax` and `builtins/mkbuiltins` and *runs* them — helper programs compiled
  for, and executed on, the **build host**.
- **Those helpers are POSIX C.** This Windows host has no POSIX C compiler: clang
  targets MSVC, so `unistd.h` does not exist (`builtins/mkbuiltins.c:33`,
  `mksyntax.c:35`), and the UCRT's `signal.h` defines `sig_atomic_t int`, which
  collides with bash's own declarations (`support/mksignames.c`).
- **configure writes host paths.** Under MSYS it emits include paths like
  `-I/c/Users/...`, which a native compiler cannot resolve (`support/man2html.c:66`
  cannot find `config.h`).

So the constraint is not the OS being built. It is that bash's build runs programs,
and those programs have to be POSIX.

## The rule

| what | build host |
|---|---|
| Rust userland, kernel, servers | any (Windows or Linux) |
| In-tree C smoke tests (`hello.c`, `ctest.c`) | any |
| Third-party configure/make C software | a **POSIX** host: Linux, WSL, or the podman container `just test-linux` already uses |

On that host `tools/cc-minix.py` is that `cc` — the command a C project's own
build system reaches, told which target it is for (`… cc-minix.py riscv64`, and
x86_64 when it is not told) — doing the same two halves `build-c-hello.py`
assembles by hand: compile with clang
`--target=<machine>-unknown-none -ffreestanding -fno-stack-protector -fno-pic`
(plus `-mno-red-zone` where the machine has a red zone, and RISC-V's
`-march=rv64gc -mabi=lp64d`, both of which `tools/ccarch.py` carries) plus the
hermetic include flags from `tools/ccflags.py`; link by compiling the C inputs and
driving the fork's rustc with the target's `tools/crt0-<arch>.S`,
`-T tools/minix-user.ld` and the `minix-libc` rlib.

The alternative, if a Windows host must stay in the loop, is to build from a
**release tarball** rather than a git checkout: the tarball ships the generated
sources, so the host tools are never built. It does not remove the host-path
problem, only that one helper.

## The Linux route: what it took

The build runs on a Fedora 44 WSL host (`wsl.exe -d FedoraLinux-44`) with the fork's
**Linux** stage1 (`rust/build/x86_64-unknown-linux-gnu/stage1`), a `cc` wrapper at
`target/tmp/cc-minix`, and `tools/c-include` for the headers. Four traps, each of
which presents as something other than its cause:

- **A git checkout on Windows is CRLF.** `core.autocrlf=true` writes every text file
  back as CRLF, so a shebang reads `/bin/sh^M` and `configure` refuses to start
  (`bad interpreter`). 1544 of 1603 files in `bash/` were affected. Converting them
  to LF leaves the tree *byte-identical to what git stores* — the index blob for
  `configure` is 670802 bytes, exactly the converted file — but `git status` then
  reports all 1544 as modified: the index still caches the CRLF size (695555) from
  the checkout and has no inode recorded. `git diff` is empty, which is the check
  that the content is right.
- **Line-ending conversion reshuffles mtimes, and make regenerates from them.** Git
  happens to write `configure` after `configure.ac`/`aclocal.m4`/`config.h.in`
  (alphabetical index order), which is the ordering make needs to leave `configure`
  alone. `sed` over the tree destroys it, so make runs `autoconf` to rebuild
  `configure` — a tool a build host may not have (Error 127). Restore it with
  `touch -d` on the three prerequisites and then on `configure`. `parse.y`/`y.tab.c`
  is the other generated/generator pair in bash's tree; there is no `Makefile.am`
  anywhere, so `Makefile.in` has no such rule.
- **The host tools need C17.** They are built by `CC_FOR_BUILD` against
  `buildconf.h`, which has no `HAVE_STDBOOL_H`, so `bashansi.h` takes its
  `typedef unsigned char bool` branch. gcc 16 defaults to C23, where `bool` is a
  keyword, so that typedef is an error. `CC_FOR_BUILD="gcc -std=gnu17"`.
- **The `minix-libc` rlib must be built by the same stage1 that links.** A rlib built
  by the Windows stage1 records a `core` that the Linux stage1 calls "possibly
  newer" (E0460), and no configure test can link. Build it in WSL with
  `RUSTC=rust/build/x86_64-unknown-linux-gnu/stage1/bin/rustc`. Two toolchains
  writing the same rlib name is the sharper form of this: see the two-builder note
  below, where the second build has to delete the first's artifact because both
  cargos believe their own build is current.

## The target compile is hermetic now, and what that exposed

`clang --target=x86_64-unknown-none -I tools/c-include` also searched
`/usr/include`, so **any header the port does not provide was silently satisfied
by glibc** and configure reported the *host's* answer as the target's:
`HAVE_UNION_WAIT`, `HAVE_TERMIOS_H` and `HAVE_STRINGS_H` all came back yes for a
target that has none of them, and bash then compiled paths no real system has.
The failure that made it visible was `locale.h`: `HAVE_STRINGS_H` came from
`/usr/include/strings.h`, which pulls `bits/types/locale_t.h`, which then
collided with the port's own `locale_t`.

The flags now live in `tools/ccflags.py`, shared by `tools/build-c-hello.py` and
the bash `cc` so the two cannot drift:

```
-nostdinc -isystem $(clang -print-resource-dir)/include -I tools/c-include
```

`-nostdinc` alone is not enough: it also drops clang's own headers, which a
freestanding compile is entitled to (`stddef.h`, `stdint.h`, `stdarg.h`, …), so
the compiler's resource dir is added back. It is asked of the compiler rather
than hardcoded because the Windows and Linux installs are different builds. No
port `limits.h` turned out to be needed — clang's own answers for this target.
The port's headers come first, so where both define a type (`stddef.h`,
`stdint.h`) the port's wins.

A missing header is now a hard error, which is the point: that is the list of
gaps, instead of a guess about what the target has. What it fixed, in order: the
`union wait` family (bash's `include/posixwait.h` typedefs `WAIT` to `union wait`
whenever `_POSIX_VERSION` is undefined, which the port's `unistd.h` never
defined), `HAVE_TERMIOS_H` going correctly to no, `clock_t` (bash asks for it
with `sys/times.h` included and writes `#define clock_t long` when that header is
absent, which then collides with the host's typedef in the *build* tools), and
the `locale_utf8locale` family — `HAVE_WCWIDTH` was no, so `HANDLE_MULTIBYTE` was
off, and bash's globbing then used an identifier that only the multibyte branch
declares. Adding `wcwidth` turned multibyte support on, which in turn asked for
`mblen`, `mbstowcs` and `mbsinit`.

## Where the bash attempt stands

**bash runs.** `configure --host=x86_64-unknown-none --disable-nls
--without-bash-malloc` completes on the Linux host, `make` compiles all 209
objects and links a 1.4 MB static x86-64 ELF with entry `0x1000000`, the address
the port loads its userland at. Injected as `/bin/bash` through `MINIXFS_EXTRA`
(the Justfile exports `MSYS2_ENV_CONV_EXCL` for it — `KNOWN_ISSUES.md`'s testing
notes record what a converted dest did: the file appeared at `/bash` while every
listing and every check looked in `/bin`) and booted, it prints its version
banner and executes: `-c 'echo BASH-OK'`, an arithmetic expansion, a `for` loop,
a redirect with a read-back, and — once `getcwd` existed — `$PWD` from its own
startup.

The steps that prove each of those, with the read-back each one needs, are
`tools/smoke/bash.tsv`, driven by `just test-bash` — the committed path, below.

What it took, beyond the hermetic compile above:

- The missing terminal interface: `termios.h`, `sys/ioctl.h`, `sys/param.h`,
  `utime.h`, `netinet/in.h`, `arpa/inet.h`, and the libc behind them — `ioctl`,
  `tcgetattr`/`tcsetattr`/`tcdrain`/`tcflush`/`tcflow`/`tcgetpgrp`/`tcsetpgrp`,
  the `cf*speed` accessors, `cfmakeraw`, `mknod`/`mkfifo`, `times`, `mktemp`,
  `setlinebuf`, `bsearch`, the BSD string names, `wcwidth`/`wcswidth`, `mblen`,
  `mbstowcs`, `wcstombs`, `mbsinit`, `htons`/`htonl`/`ntohs`/`ntohl` and
  `inet_addr`/`inet_aton`/`inet_ntoa`. Most of the *kernel* side was already
  there (the tty server has the NetBSD `TIOCGETA` family and `do_mknod`); the C
  surface was what was missing.
- The two configure knobs a static libc makes necessary (see the traps list):
  `--without-bash-malloc` and `-DNEED_EXTERN_PC`.
- `_POSIX_VERSION` in `unistd.h`: bash's `include/posixwait.h` typedefs `WAIT` to
  `union wait` whenever it is undefined, which is 10 errors that are not about
  glibc at all.
- `environ` and a real `getenv`: the libc had a stub returning NULL, so **every C
  program saw an empty environment**. `crt0` now publishes the `envp` it was
  handed through `__minix_set_environ`, and `environ` points at an empty array
  before that so a constructor can dereference it.

**The environment survives `exec` now.** It did not, until this session: `execve`
in `crates/minix-libc/src/c_sys.rs` took `_envp` and ignored it,
`minix_std::process::exec` had no environment parameter at all (it passed null),
and the kernel's frame builder therefore never received one — while
`minix_rt::execve` and the kernel's frame *parser* already carried `envp`. Half
the work was built and nothing used it. Now the C `execve` counts `envp` (capped
at 63, as the frame is) and passes it, `minix_std::process::exec` forwards it,
and `crt0` publishes the block as `environ` before `main`. Verified in an x86
guest: `tools/ctest.c` re-execs itself with `CTESTENV=hello` and the child prints
`getenv: hello`. bash needs exactly this for `PATH`/`HOME`/`TERM` and for
handing them to its children.

**The two gaps the run found**, and what each was hiding behind:

- **The shell did not remove quotes at all.** `crates/userland/src/shell.rs`
split a line with `split_whitespace()`, so a quoted argument reached its program
as fragments: `bash -c 'echo BASH-OK'` handed bash `'echo` and `BASH-OK'`, and
bash's reply — ``BASH-OK': -c: line 1: unexpected EOF while looking for matching
`''`` — reads as a defect in bash. No scenario or probe in the tree had ever
used a quote, which is why it survived. The tokenizer is quote-aware now
(`shell.rs::tokenize`): `'…'` and `"…"` are stripped in place, whitespace
inside them stays in its word, a backslash escapes the byte after it, `echo ''`
still passes one empty argument, and an unclosed quote is refused
(`sh: unexpected EOF while looking for matching `'`) rather than run with the
quote as text.
- **`getcwd` was a stub returning `ENOSYS`.** VFS holds a directory's *vnode*,
not its name, so the name is recovered by walking up — `stat(".")` and
`stat("..")` identify the directory, a scan of `".."` finds which entry holds
that inode, `chdir("..")` moves up for the next round, and root is where the two
stats agree — which is what MINIX's `__getcwd` does; the port's walk is ported
from it, and restores the caller's cwd on every path, including failure. The
allocate form `getcwd(NULL, size)` is part of it because that is how bash asks
(`getcwd(0, PATH_MAX)` in `builtins/common.c`). Measured in an x86 guest:
`getcwd=/tmp/cwdtest errno=0`, the relative-path write that follows the walk
lands in `/tmp/cwdtest`, and bash now has a cwd of its own —
`/bin/bash -c 'printf "PWD=%s\n" "$PWD"'` prints `PWD=/tmp/cwdtest` where
before it printed nothing, and the `shell-init: error retrieving current
directory` line is gone.

The first symptom pointed at neither: the failure read `shell-init: error
retrieving current directory: getcwd: cannot access parent directories: Unknown
error`. The missing call was `getcwd`; the *name* of the failure was missing
from `strerror`, whose table stopped at 34 (see the errno note at the end).

**The committed path.** The build is tracked now — `tools/build-bash.py` fetches
bash at a pinned upstream commit (a git checkout rather than a release tarball:
that is what the working build was validated against, and the generated files the
tarball would add are the ones whose regeneration the traps below are about),
rebuilds `minix-libc` with the host's own stage1, configures, links and publishes
`target/bash/<arch>/bash`. On Windows it re-enters WSL by itself (`MINIX_WSL_DISTRO`
picks the distribution), because the stage1, the rlib and clang have to be the
same host's — which is the whole of why this route is Linux.

`just build-bash <arch>` runs it (the artifact lands at `target/bash/<arch>/bash`);
`just test-bash <arch>` then injects the result as `/bin/bash`
through `MINIXFS_EXTRA` (still not a `BOOT_BINS` entry: an image that always
carried a 1.4 MB bash would only build where bash had been built) and drives
`tools/smoke/bash.tsv` at it in the guest — the banner, `-c`, arithmetic, a loop,
a redirect read back through a second process, an external command (bash's
fork+exec path) and `$PWD` from its own startup. CI's `bash` job runs that pair for
each of the three arches and publishes the shell as the `bash-<arch>` artifact; the
release workflow downloads it and injects the same `/bin/bash` into the image it
publishes, so a released image carries a shell for scripts (a plain `just image`
stays bash-free). The job is in the release's `needs`, so a C surface that builds
bash but breaks it blocks a release.

**What that still does not cover:** an *interactive* bash. The driver types into
the minix shell and waits for its `#` prompt, which bash's `bash-5.3#` replaces,
so every step is a fresh `bash -c`. Its profile files, its terminal setup
(`tcsetattr` into raw mode) and readline are therefore still only as exercised as
typing at it by hand once — the one part of bash a scenario cannot reach yet.

## A three-arch C surface

The **compile** half was always arch-generic: clang is a cross compiler, and
`tools/c-include` is little-endian headers with nothing x86 in them beyond the
comments — `tools/hello.c` compiles for `x86_64-unknown-none`,
`riscv64-unknown-none` and `aarch64-unknown-none` alike. The **link** half was
x86_64's alone (`tools/crt0-x86_64.S` and nothing beside it, five libc entry
points written in x86 asm, `jmp_buf` the SysV x86_64 layout). That is what
changed, and this is what it took:

- **The entry points.** `tools/crt0-aarch64.S` and `tools/crt0-riscv64.S` follow
  the same sequence as the x86 one: read `argc`/`argv`/`envp` off the initial
  stack, park them in callee-saved registers across `minix_libc_tls_init`,
  publish `environ`, run `.init_array`, call `main`, `exit`. The per-arch part is
  the register set and the stack layout, which
  `rust/library/std/src/sys/pal/minix` already documents for its own `_start` —
  that is the reference to crib from.
- **`setjmp`/`longjmp`** (`crates/minix-libc/src/c_setjmp.rs`) save what each ABI
  calls callee-saved: x86_64's rbx/rbp/r12-r15/rsp/rip, aarch64's x19-x30/sp and
  d8-d15, riscv64's ra/sp/s0-s11 and fs0-fs11. `tools/c-include/setjmp.h`
  declares the matching `jmp_buf` length, and the two have to agree: bash reaches
  both through its `posixjmp.h`.
- **`strtold`, `strtold_l`, `wcstold`** are asm on x86_64 for one reason only:
  that ABI returns an 80-bit `long double` in x87 ST0. On aarch64 and riscv64
  `long double` *is* `double`, so each is a one-line call to `strtod`/`wcstod`.
- **The RISC-V float ABI is the one thing clang's default gets wrong** for this
  port: `riscv64-unknown-none` defaults to soft-float, the fork's
  `riscv64gc-unknown-minix` spec is `Lp64d`, and lld refuses to link objects whose
  `EF_RISCV_FLOAT_ABI` differ — which is a correctness question before it is a
  link error, since a `double` would cross the Rust/C boundary in a register one
  side does not use. `-march=rv64gc -mabi=lp64d` is the fix. `-mno-red-zone` is
  accepted-but-unused on RISC-V, so it is passed only where a red zone exists.
  Both live in `tools/ccarch.py`, which the three drivers share
  (`cc-minix.py`, `build-c-hello.py`, `build-bash.py`), so a further target is one
  entry rather than three.

Verified: `hello.c` and `ctest.c` link for all three targets; bash's configure,
its 209 objects and its link succeed for all three (`just build-bash <arch>`); and
bash's own smoke scenario passes in the guest on x86_64 and aarch64.

**riscv64 was the one that stopped short, and the reason was one field.** In a
riscv64 guest `/bin/bash --version` printed nothing and never returned, and so did
every `sscanf` — with or without an argument read, and through `vsscanf` given a
`va_list` the C compiler built. That was never a scan problem: `psl::PSL_USERSET`
enabled the FP unit with `FS=Initial`, which RISC-V lets an implementation treat as
`FS=Off`, so no process had floating point at all and the first instruction rustc
spills in *any* function that touches a `double` (`c.fsdsp` in the prologue) was an
illegal instruction. The trap handler's catch-all arm answered that with a silent
`wfi`, which is why the symptom was a hung guest rather than a fault: the guest was
idle and nothing said why. `FS=Dirty` is the value that means "the registers hold
the current state" — and at that point nothing kept an image of `f0`-`f31` to
restore from, the x86 FPU support being `kernel::fpu` and x86-only (RISC-V has one
now; see "riscv64's per-process FP image" below) — so the unit comes up on,
the trap return writes the same `PSL_USERSET` for every U-mode return, and the
catch-all arm names the cause, `stval`, `sepc` and the mode before it halts. bash's
banner, arithmetic, loops, redirection and fork+exec all run in a riscv64 guest now.

What the FP fix then exposed is that a *long-running* C program still dies, and the
cause was the port's C heap rather than the kernel: `realloc` read a block's size
from the block header at the *user* pointer instead of `HDR` bytes below it, so a
grow that read high returned the same too-small block and the caller overran it;
and `malloc`'s extension path placed a fresh block at a page-rounded `sbrk` address
while leaving the free-list walk unable to cross the gap in front of it, so every
extension leaked its page (and `free`'s walk back from the heap start, which is
O(heap), then made the leak look like a hang). Both are fixed
(`crates/minix-libc/src/lib.rs`) and the difference is measurable in the guest: a C
probe that grows one buffer to 100000 bytes prints `REALLOC ok`, a churn probe of
300000 varying-size `malloc`/`free` cycles goes from hanging to `CHURN ok 300000`,
and `bash -c 'for i in {1..4000}; do s=$((s+i)); done; echo S=$s'` goes from killed
by SIGSEGV every time to `S=8002000`. A prompt-paced scenario of 30 `bash -c` steps
lost 5-6 steps before those fixes and 1 after.

What was left after that was a trap-state bug, not a C one. A `bash -c` step still
died in about 5-10% of invocations — the child ended as a signal (139) and VM's
trace of the kill said `pf-noregion 0x0`, error code 0x14, a user *instruction*
fetch at address zero, which reads as a call through a null function pointer. It
was not one: the riscv64 trap handler read `stval` live, and `stval` is the one
fault CSR that is not part of the interrupted context — the entry saves `sepc`,
`sstatus` and `scause` in the frame but not `stval`, and the architecture lets a
trap that is not a load/store/instruction fault (an interrupt, and this port takes
its ticks inside a U-mode trap's handler) write 0 there. VM was handed address 0,
found no region covering it, and killed a process that had done nothing wrong.
`trap_asm.rs` now saves `stval` in the trap frame's unused `288..296` slot and
`trap.rs` reads the fault address from the frame. With that, 60 consecutive
`bash -c` invocations in the prompt-paced census lost nothing, `just test-bash
riscv64` is green, and so are `test-qemu`/`test-boot riscv64` and the workspace
suite.

**riscv64's per-process FP image.** The trap frame saves no `f`-register, so with
`FS=Dirty` two processes that each use floating point can see each other's
registers across a switch — and there is no RISC-V lazy `FS=Off`-and-trap arm to
fall back on. Measured first, with a C probe that names `fs0`, holds a value in it
across a `getpid()` and checks it afterwards: two processes in flight reported
`FPPROBE parent bad=40000` and `FPPROBE child bad=40000` (every round, both),
while the same probe without the `fork` reported `FPONE bad=0` — the loss needs the
*other* process to have run, not just a trap. A first version of the probe passed
instead: `register double held asm("fs0")` around the call compiled to `fsd fs0`
before it and `fld fs0` after it, giving the value a stack home the compiler
reloaded, so the register never really held it. The set, the call and the check are
one `asm` block now.

The fix saves at the switch, not the entry, because the kernel executes no floating
point itself: nothing between a process's last user instruction and the switch can
disturb its registers, so the switch sites — `riscv_post_syscall`,
`riscv_timer_callback` and the boot `switch_to_user` in
`crates/kernel-boot/src/riscv64.rs` — call `kernel::fpu::save` on the process being
left and `restore` on the one being entered. `fork` saves the parent before copying
the image to the child, `exec` zeroes it, and `restore` writes `fcsr` (the rounding
mode and the accrued flags) as well as `f0`-`f31`, so neither leaks either. The
registers are the first 256 bytes of the one-page area; `fcsr` sits just past them
(`RISCV_FCSR_OFF`), while the user-visible `mc_fpstate` stays registers-only as the
mcontext ABI defines it. With it the probe reports `FPPROBE ok` (both `bad=0`), the
prompt-paced census is 80/80 with zero losses, `just test-bash` is 6/6 on all three
arches, and `test-qemu`/`test-boot riscv64` and the workspace suite are green.

bash is reliable in a riscv64 guest for everything the smoke scenario and the
census cover, and floating point in user programs now survives a context switch.

**One more thing the C surface did not have: a clock.** The riscv64 timer was
armed — `clint::init_timer` writes `stimecmp` — and never enabled: nothing in the
port set `sie.STIE`, so the deadline lapsed in silence. No tick, no virtual timer,
no `alarm` (a probe that spins with a three-second `alarm` is never interrupted),
nothing preempted, and the boot-progress watch, which counts ticks, never reached
its deadline — which is why `just test-boot riscv64` stopped at `phase 1 complete;
releasing init`. The per-tick accounting had a second reason never to run: it sat
behind the timer callback's SPP check, and every tick this port takes arrives in
S-mode (sampled at the 1st, 100th, 1000th and 10000th tick), because a U-mode
trap's handler runs with `SIE=1`. Both halves are fixed: `sie.STIE` is set where
the timer is armed, and the accounting runs for either mode while the SPP check
still decides whether the tick may switch. `just test-boot riscv64` now prints
`ALL TESTS PASSED`.

**The multicall now travels with two of the images.** `/bin/coreutils` (the uutils
multicall, 60 applets under `feat_minix`) was embedded on x86_64 alone: `just
coreutils-riscv64` and `just coreutils-aarch64` existed but nothing depended on them
and neither copied its binary into the directory the kernel embeds from, so no image
they built could carry it. Both copy it there now and `build-riscv64`/`build-aarch64`
depend on them — except aarch64, which is deliberately left out (below).

riscv64's ships: the same `test-coreutils-wedge` scenario the x86 gate runs passes
10/10 in a riscv64 guest. aarch64's does not, so it is built but not embedded:
`coreutils seq 3` prints, but `seq 20` (and 100/200/1000) intermittently writes
*nothing at all* — an empty file, no error, no signal the shell reports — and the wedge
scenario failed 0/3 aarch64 boots (KNOWN_ISSUES aarch64 #9 has the measurements). The
exclusion is one line in `crates/kernel/build.rs`.

aarch64 is also the only target whose multicall needs a C compiler at all: blake3's
aarch64 path compiles `blake3_neon.c` unconditionally, where x86 falls back to Rust
intrinsics when no `cc` is present and riscv64 has no SIMD path. `coreutils-aarch64`
runs it under the port's own `tools/cc-minix.py aarch64` (with `llvm-ar` where there is
no `ar`, `ar` otherwise) — the same hermetic C compile bash's build uses.

## Traps

- **The rlib has two builders, and each thinks its own build is current.**
  `tools/build-c-hello.py` links with the *Windows* stage1, `tools/cc-minix.py`
  with the *Linux* one, and both write into `target/x86_64-pc-minix/release/deps/`.
  Measured: they do **not** always share a filename — editing the libc changed the
  hash, and two `libminix_libc-<hash>.rlib` files then sat in `deps/` at once. The
  wrapper takes the newest by mtime, so a `just build-x86` (Windows) run *after*
  the Linux one makes it pick the Windows-built rlib, which depends on the
  Windows-built `core`, and the link fails with `E0460: found possibly newer
  version of crate core`. The asymmetry is the rest of the trap: the *Linux* build
  must delete the rlib first (`rm -f
  target/x86_64-pc-minix/release/deps/libminix_libc-*.rlib` before rebuilding)
  because its fingerprint says "current" and it skips the build, while the Windows
  build recompiles by itself, having seen the output change. `tools/build-bash.py`
  does that deletion itself; keep its order — delete, build with the Linux stage1,
  then link — with no Windows minix build in between.
- **`cargo clean` takes the bash source with it.** `bootstrap` and `fetch-stage1`
  both begin with one, and everything bash's build keeps under `target/`
  (`bash-src`, `bash-build`, the artifact) is inside what it removes. So build
  bash *after* the toolchain: otherwise the next `just build-bash` refetches and
  reconfigures from scratch, with no error to say why.
- **`make` reports success without relinking when only the libc changed.** The
  rlib is not a prerequisite of any bash target, so with every `.o` newer than
  `bash`, `make` does nothing and says so: `make exit: 0`, 203 objects, and the
  binary's mtime and size unchanged. Its output is identical to a real relink, so
  the binary's mtime is the only evidence — check it. Force the relink (`rm -f
  target/tmp/bash-build-linux/bash` before `make`, as the scratch script's caller
  does) whenever the libc changes, or you will boot a bash built against the
  previous one and read its behaviour as the new one's. `KNOWN_ISSUES.md`'s
  testing notes record the same shape for the kernel variants, where a
  "Finished" once meant nothing had relinked.
- **Two collisions a static libc causes that a shared one hides.** Both are
  configure choices rather than code changes, and both cost a link:
  `--without-bash-malloc`, because bash's own `lib/malloc` and the libc's malloc
  are then both in the link and lld refuses the duplicate (a shared libc would
  simply be interposed); and `-DNEED_EXTERN_PC`, because readline defines
  `PC`/`BC`/`UP` for systems whose termcap comes from a curses library
  (`lib/readline/terminal.c`, guarded on `!__linux__ && !NCURSES_VERSION`) while
  bash's bundled `lib/termcap` defines the same three — `NEED_EXTERN_PC` is the
  macro readline provides to turn its definitions into declarations.
- **A `cc` wrapper that drops the link flags links an empty executable.**
  `target/tmp/cc-minix` compiled and linked, but passed only `.o`/`.a` inputs to
  the linker, so `-lbuiltins -lglob -lsh -lreadline -lhistory -ltilde -lmalloc`
  from make's link line went nowhere and every symbol from those archives came
  back undefined. `tools/ccflags.link_passthrough` now classifies a `cc` line's
  arguments, so the knowledge lives in the tracked file and not in the wrapper.
  A related artifact: while `-ldl` was being dropped, configure's
  `AC_CHECK_LIB(dl, dlopen)` *passed* (dlopen resolved from the rlib) and wrote
  `LIBS=-ldl`; once the flags were forwarded, the check failed honestly and the
  flag disappeared on the next configure.

## The headers: generated, and the two sides now agree

`tools/gen-c-headers.py` derives the C headers from the libc with cbindgen and
writes them to `target/c-include/` (13 headers), and `tools/check-c-headers.py`
asserts the two sides agree in both directions. It reports 0 in each direction:
386 exports, 382 declarations.

The checker had been under-reporting, which is what its own output made visible:

- A prototype that wraps over two lines was not a declaration to it, so
  `pthread_create`, `qsort`, `sendto` and a dozen more were reported undeclared
  while sitting in the headers. It now accumulates a declaration until its `;`.
- A struct member ends in `;` and starts with an identifier, so `d_ino`,
  `pw_name`, `sa_family` and the rest read as declarations. Bodies are skipped now.
- `extern "C" {` opens a brace, and tracking it swallowed whole headers.

What it then reported was real. `bsearch` was declared in `stdlib.h` and never
implemented — a link error waiting for the first C caller — and is implemented.
Eleven exports were declared nowhere a C caller looks, i.e. the tree's headers
lagged the libc: `issetugid`, `setegid`, `seteuid`, `setgroups`, `logb`,
`pthread_kill`, `utime`, `utimes`, `vsscanf`, `vfscanf`, `vscanf`. All are
declared now (`utime.h` and `sys/times.h` are new headers), which is what took the
check to zero. `strings.h` gained the BSD names because the libc implements them
now rather than because a caller wanted them.

The reason the two header sets still have to be reconciled by hand is that
cbindgen derives declarations, not constants: `EOF`, `SEEK_SET`, the `errno`
numbers and the struct layouts have no Rust export to come from. So retiring
`tools/c-include` is not a deletion but a merge — each hand-authored header keeps
its types and macros and takes the generated declarations, and the check then
reads the set the compiler actually gets. Until that happens the generated headers
are a drift detector, not the include dir. `MODULE_HEADER` and `SYMBOL_HEADER` in
the generator are the per-symbol table the note above used to call "the next
refinement"; they place `ioctl`, `mknod`, `mkfifo` and the `inet_*` family in
`sys/ioctl.h`, `sys/stat.h` and `arpa/inet.h` rather than in their module's
default header.

## A note on errno numbering

This port numbers its C `errno` values the way Linux does — `minix-std` returns
`EAGAIN = -11` and `ENOTCONN = -107` — not the way the MINIX reference does
(`sys/sys/errno.h` puts `EAGAIN` at 35). `tools/c-include/errno.h` therefore
follows the port's numbers, not the reference's. The two agree on `ESPIPE` (29)
and disagree on several others, so copying the reference's values there would be
wrong.

The mix is easy to get wrong in the other direction too, and it cost a real
diagnosis: `strerror`'s table stopped at 34, so `strerror(ENOSYS)` — 78 in this
numbering — returned "Unknown error", and the `getcwd` failure above was
reported with the wrong name attached to it. The table now carries a message for
every errno the header declares, and a host test reads `errno.h` and asserts it
(`c_string::tests::every_declared_errno_has_a_message`), so the table cannot
fall behind the header again without the test naming the errno that did it. The
numbers `errno.h` does not declare stay empty rather than borrowing a
neighbouring system's text, because this numbering is not one system's. A second
test holds every message to the `strerror` buffer's size: the buffer was 32
bytes, which silently truncated the longest messages, i.e. reported a different
failure than the one that happened.
