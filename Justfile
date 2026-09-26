# Build and run the MINIX/Rust port in QEMU.
#
# Targets: x86 (default), riscv64, aarch64.
#
# Requires:
#   - `just bootstrap` first (builds the rust fork stage1 compiler, which
#     provides the in-tree minix targets + their std sysroot)
#   - QEMU 11 or newer (the suite recipes refuse an older emulator - see
#     `qemu-min-version`) and clang on PATH — clang assembles the x86 trampoline
#     and the C/C++ smoke tests; the toolchain's own tools (lld, nm, objcopy) are
#     resolved from its build tree, with PATH only as a fallback (see `minix-lld`
#     below)
#
# The recipes orchestrate plain `cargo` invocations; all image assembly
# (initramfs CPIO + MinixFS) lives in `crates/kernel/build.rs`. x86
# post-link work (trampoline + kernel.bin) lives in `tools/mkboot.rs`.

# Path to the rust fork's stage1 rustc (built by `just bootstrap`); used as
# the RUSTC for userland/server builds so the in-tree minix targets and
# their std sysroot are used (no `-Zbuild-std`, no JSON specs).
stage1-rustc := `ls rust/build/*/stage1/bin/rustc.exe rust/build/*/stage1/bin/rustc 2>/dev/null | head -1`

# Path to an lld that can link the minix targets, resolved by `tools/lld.py`
# (the toolchain's own when it has one, a system LLVM otherwise). The minix
# target specs make rustc run a program named `lld`, which a CI image does not
# have on PATH. Empty when no lld exists yet - `just bootstrap` links only
# after x.py has built one, so the scripts that link resolve it themselves.
minix-lld := `python tools/lld.py`

# The per-target `linker` settings cargo reads (`CARGO_TARGET_<TRIPLE>_LINKER`),
# rather than RUSTFLAGS: RUSTFLAGS also reaches the host build scripts, which
# must keep the host linker. The scripts that invoke rustc directly pass the
# same path with `-C linker=`.
export CARGO_TARGET_X86_64_PC_MINIX_LINKER := minix-lld
export CARGO_TARGET_RISCV64GC_UNKNOWN_MINIX_LINKER := minix-lld
export CARGO_TARGET_AARCH64_UNKNOWN_MINIX_LINKER := minix-lld

# MSYS converts POSIX-style env values when an MSYS shell starts a *native*
# tool, and `MINIXFS_EXTRA`'s value (`/bin/bash=<path>`) is one. Converted, its
# dest is no longer `/bin/...`, and boot-image::minixfs routes a dest it does
# not recognise to the root filesystem — the file appears at `/bash` and every
# listing of `/bin` says it is missing. (`crates/kernel/build.rs` refuses that
# dest now, so it is a build error rather than a misleading image.)
#
# The exclusion has to be in the environment of the shell doing the spawning,
# which is why this is exported for every recipe and not prefixed onto the lines
# that spawn a native tool: the recipes that re-enter `just` (`run` -> `run-x86`,
# `image` -> `image-x86`, `build` -> `build-x86`, ...) have the value converted
# when the inner `just.exe` starts, before any line of the inner recipe runs.
# The list is semicolon-separated; `DYNLINK_BINS` has the same hazard as
# `MINIXFS_EXTRA` (`/libexec/ld.so=<path>;...` is a POSIX-style value too).
export MSYS2_ENV_CONV_EXCL := "MINIXFS_EXTRA;DYNLINK_BINS"

# Repo root with forward slashes (the recipe shell mangles backslashes).
ROOT := replace(justfile_directory(), "\\", "/")

# Rust channel pinned by rust-toolchain.toml; also the tag of the container
# image `test-linux` uses, so the two cannot drift apart.
rust-channel := `sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml | tr -d '\r'`

# Fetches the build-critical submodules (the rust fork and the uutils
# coreutils source) when they're missing — `git submodule update --init` is a
# no-op when they're already present and at the pinned commits — regenerates
# `rust/config.toml` via `tools/rust-config.py` for the requested arch, then
# runs x.py and builds the `/bin/hello` std smoke-test binary with
# `tools/build-std-hello.py`.
# Note: per-arch runs rebuild only that arch's std — x.py prunes the other
# arches from the stage1 sysroot — so `all` (the default) is the complete
# setup. Incremental afterwards; the first run downloads the stage0
# toolchain and CI LLVM (needs network).
# Build the stage1 compiler + std + /bin/hello for a minix target (all by default).
bootstrap target="all":
    git submodule update --init rust coreutils
    python tools/rust-config.py {{target}}
    # `library/proc_macro` is built alongside std: x.py prunes the stage1
    # sysroot to the listed crates, and without libproc_macro a later host
    # proc-macro build (the coreutils multicall) fails with E0463.
    # Stage 1 is spelled out because x.py asserts an implicit stage is 2 under
    # CI, and stage 1 is the compiler every other recipe consumes.
    cd rust && python x.py build --stage 1 library/std library/proc_macro
    @just _finish-bootstrap {{target}}
    @echo "stage1 compiler + /bin/hello ready. Rebuild the images: just build-{{ if target == "all" { "x86" } else { target } }} && just mkfs-{{ if target == "all" { "x86" } else { target } }}; boot with just run-{{ if target == "all" { "x86" } else { target } }} 256M"

# Bring the tree in line with a stage1 sysroot that already exists: drop the
# cargo cache, then rebuild the smoke-test binaries the clean removed.
# x.py rebuilt the stage1 rustc, but cargo fingerprints the compiler by version
# string - an incremental rebuild keeps the same string, so the old rlib cache
# stays "fresh" and the next userland build fails with E0463 ("can't find crate
# for ...") when rustc cannot read the stale metadata.
# coreutils is a workspace of its own, with its own target/ dir, so that clean
# never reaches it: without the clean below the multicall links rlibs built by
# the previous toolchain against the new sysroot, which shows up as the same
# E0463 and as a mixed `TypeId` scheme - clap then panics in
# `MatchesError::Downcast` on every flag it reads. The price is one full
# multicall rebuild after a bootstrap.
_finish-bootstrap target:
    cargo clean
    # `fetch-stage1` does not require the coreutils submodule, so skip this
    # where its manifest is not checked out rather than fail there.
    if [ -f coreutils/Cargo.toml ]; then cargo clean --manifest-path coreutils/Cargo.toml; fi
    # A clean that quietly does nothing (an unparseable manifest is enough for
    # it to report success) leaves the old rlibs behind, so assert it took
    # effect now rather than let it resurface minutes into a later build.
    @test -z "$(find coreutils/target -name '*.rlib' 2>/dev/null | head -1)" || (echo 'error: stale coreutils rlibs survived the clean - they fail later builds as E0463 or as clap TypeId panics' >&2 && exit 1)
    python tools/build-std-hello.py {{target}}
    # The C smoke-test binaries (helloc/ctest) also live under target/ and are
    # wiped by the clean. They build for all three arches now
    # (`just build-c-hello <arch>`), but the boot images embed them on x86_64
    # only (`crates/kernel/build.rs`), so only that one is rebuilt here.
    if [ "{{target}}" = x86 -o "{{target}}" = all ]; then python tools/build-c-hello.py x86; fi

# Install the prebuilt stage1 toolchain for the commit the `rust` submodule
# pins, fetched from a release on the rust fork and checksum-verified, instead
# of building LLVM and the compiler from source. It is the same toolchain
# `bootstrap all` produces (host + all three minix targets) and is therefore
# host-specific; `bootstrap` stays authoritative for work inside the fork.
# Fetch the prebuilt stage1 toolchain instead of building it from source (`bootstrap`).
# A published release can be verified by consuming it: `just verify-stage1`
# (Linux/WSL only - the asset is host-specific).
fetch-stage1 target="all":
    python tools/fetch-stage1.py
    @just _finish-bootstrap {{target}}
    @echo "prebuilt stage1 ready. Rebuild the images: just build-{{ if target == "all" { "x86" } else { target } }} && just mkfs-{{ if target == "all" { "x86" } else { target } }}; boot with just run-{{ if target == "all" { "x86" } else { target } }} 256M"

# Consume the published stage1 the way a Linux dev or CI job does: fetch it (into
# target/stage1-verify), link the smoke binaries for all three minix targets with
# it, and check each result's ELF machine. Fails on Windows because the published
# asset is a Linux one; run it in WSL there.
verify-stage1:
    python tools/verify-stage1.py

# Userland + server binaries for a target, built into the shared cargo
# target dir (fast incremental; required before the kernel build, whose
# build.rs assembles the images from them).
userland-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    @test -n "{{minix-lld}}" || (echo 'error: no lld to link the minix binaries with — run `just bootstrap`/`just fetch-stage1`, or install LLVM' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target x86_64-pc-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target x86_64-pc-minix --release

userland-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    @test -n "{{minix-lld}}" || (echo 'error: no lld to link the minix binaries with — run `just bootstrap`/`just fetch-stage1`, or install LLVM' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target riscv64gc-unknown-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target riscv64gc-unknown-minix --release

userland-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    @test -n "{{minix-lld}}" || (echo 'error: no lld to link the minix binaries with — run `just bootstrap`/`just fetch-stage1`, or install LLVM' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target aarch64-unknown-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target aarch64-unknown-minix --release

# uutils coreutils multicall (/bin/coreutils) for a minix target, built from
# the coreutils submodule with the feat_minix feature set. -C strip=symbols
# and -C opt-level=z keep the 56-tool binary ~6 MiB so it fits the default
# 16 MiB minixfs. The getrandom backend cfg routes rand users (shuf/sort/
# factor) to the weak RNG registered in src/bin/coreutils.rs until the OS
# grows a kernel entropy source.
#
# NB: cargo chdirs into the submodule for --manifest-path builds, so lld
# resolves the linker script relative to coreutils/ (-T../tools/...) and the
# binary lands in coreutils/target — copy it into the shared target dir the
# kernel build.rs embeds from.
coreutils-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-T../tools/minix-user.ld -C link-arg=--no-eh-frame-hdr --cfg getrandom_backend=\"custom\" -C strip=symbols -C opt-level=z" cargo build --manifest-path coreutils/Cargo.toml --release --target x86_64-pc-minix --no-default-features --features feat_minix
    cp coreutils/target/x86_64-pc-minix/release/coreutils target/x86_64-pc-minix/release/coreutils

coreutils-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-T../tools/minix-user.ld -C link-arg=--no-eh-frame-hdr --cfg getrandom_backend=\"custom\" -C strip=symbols -C opt-level=z" cargo build --manifest-path coreutils/Cargo.toml --release --target riscv64gc-unknown-minix --no-default-features --features feat_minix
    cp coreutils/target/riscv64gc-unknown-minix/release/coreutils target/riscv64gc-unknown-minix/release/coreutils

# Built (and needed to reproduce the bug) but *not* embedded: the aarch64 multicall
# intermittently writes no output at all — see KNOWN_ISSUES aarch64 #9 — so the image
# does not carry it yet.
coreutils-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    # blake3's aarch64 path compiles `blake3_neon.c` unconditionally, while x86 falls
    # back to Rust intrinsics when no `cc` is found and riscv64 has no SIMD path at
    # all — so aarch64 is the one target whose multicall needs a C compiler. It is the
    # port's own, told the arch (`tools/cc-minix.py`), with an archiver for the object:
    # `llvm-ar` where it exists (MSYS has no `ar`), binutils' `ar` otherwise (a Linux
    # runner). Both are named bare, so cc-rs resolves them on PATH.
    AR_aarch64_unknown_minix="$(command -v llvm-ar >/dev/null 2>&1 && echo llvm-ar || echo ar)" CC_aarch64_unknown_minix="python3 {{ROOT}}/tools/cc-minix.py aarch64" RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-T../tools/minix-user.ld -C link-arg=--no-eh-frame-hdr --cfg getrandom_backend=\"custom\" -C strip=symbols -C opt-level=z" cargo build --manifest-path coreutils/Cargo.toml --release --target aarch64-unknown-minix --no-default-features --features feat_minix
    cp coreutils/target/aarch64-unknown-minix/release/coreutils target/aarch64-unknown-minix/release/coreutils

# ---------- build ----------

build target="x86":
    @just build-{{target}}

# build-x86* embeds /bin/coreutils, so the multicall must be built and copied
# into the shared target dir first.
build-x86: userland-x86 coreutils-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs

build-x86-test: userland-x86 coreutils-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,integration-tests kernel-test

build-x86-boot: userland-x86 coreutils-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,boot-test kernel-boot

build-riscv64: userland-riscv64 coreutils-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64 --release

build-aarch64: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64 --release

# ---------- run ----------

run target="x86" memory="256M":
    @just run-{{target}} {{memory}}

run-x86 memory: build-x86 mkfs-x86
    qemu-system-x86_64 -nographic -m {{memory}} -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0

run-riscv64 memory: build-riscv64 mkfs-riscv64
    qemu-system-riscv64 -machine virt -m {{memory}} -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64

run-aarch64 memory: build-aarch64 mkfs-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m {{memory}} -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# One self-contained, bootable artifact per arch: the kernel with the initramfs
# and root filesystem embedded, so QEMU needs no separate disk. With no virtio
# disk attached, MFS mounts the embedded image through the ramdisk driver
# (`fs::block_io::bdev_driver_root` falls back to it when the preferred driver
# has no device).
#
# The artifact is an ELF, which is what `-kernel` loads; x86 carries the kernel
# as a segment of its multiboot trampoline ELF, the other arches are the kernel
# itself. A raw disk image for real hardware would instead be the (unwired)
# `tools/mbr.S` + `tools/stage2.S` + `tools/mkimg.rs` route.
#
#  Each recipe boots what it produced and drives the userspace smoke scenario into its
# shell, so a bad artifact fails here rather than in someone's hands. The boot timeout
# bounds the guest - 15 s, against a guest that reaches the shell in about two, with the
# scenario's own budget set a second inside it - so a guest that stops making progress
# fails the recipe instead of only being slow. It is a parameter so that the release job
# (`.github/workflows/image-release.yml`, called by `ci.yml`) can pass a larger one: a CI
# runner emulates the guest, and that is a property of the runner, not of the artifact.
#
# The QEMU command mirrors the matching `run-*` recipe minus the disk - which is
# the point - so the display and input devices are passed too: without virtio-gpu
# `fb` fails to initialise on riscv64/aarch64 and wserver has no framebuffer (x86
# would still look fine, because QEMU keeps a default VGA adapter under
# `-nographic`).
#
# `/usr/bin/timeout` is spelled out because on Windows a bare `timeout` resolves
# to the Windows TIMEOUT.EXE instead: a recipe shell inherits System32 ahead of
# the MSYS bin dir, and that binary's command line is unrelated (the same trap
# applies to `find`, `sort` and `more`).
# Build the single-file boot artifact for an arch. `boot-timeout` is what the
# self-check gets (see the note above).
image target="x86" boot-timeout="15":
    @just image-{{target}} {{boot-timeout}}

# One self-contained, bootable ELF per arch, booted and driven through the userspace smoke scenario
# (`tools/smoke/scenario.tsv`): a program exec'd from the image, a file written, that file read back
# — the same steps `run.js` drives in the browser. The guest is not left to print markers on its own;
# the scenario is typed into the shell that a user gets, which is what makes this a check on the
# artifact rather than on the servers having started. `wserver: ready` stays an assertion as well:
# it caught a real regression (a browser-ready desktop, dead on the arches) that the scenario could
# not see — the shell worked while the window server was stuck, finding 66.
image-x86 boot-timeout="15": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    sh tools/smoke/feed.sh target/image-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @just _assert-qemu-log target/image-x86.log "wserver: ready"
    @echo "done: target/images/x86_64-pc-minix/minix-x86.elf — qemu-system-x86_64 -nographic -m 256M -kernel <it>"

# Acceptance test for KNOWN_ISSUES.md 12 — `coreutils seq 3` must write its three lines and
# the next tool must read them back. It is the gate that made the multicall wedge findable: a
# child used to die inside `clap` on a corrupted pointer (item 12's open half), and these steps
# are exactly what that defect broke.
#
# Green as of 2026-09-22: the last cause was the kernel clobbering a live process's XMM
# registers on entry (`crates/kernel/src/fpu.rs`), and the recipe went green with no edit to
# these steps, which is what it was written to do. Kept separate from `image-x86`'s boot smoke
# so that gate keeps measuring what it means to (that the image boots), and out of
# `test-arches`, as it was while red, so a failure here is never reported as a boot or arch
# failure.
test-coreutils-wedge boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/coreutils-wedge.tsv sh tools/smoke/feed.sh target/test-coreutils-wedge.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "coreutils-wedge: no child was left wedged"

# Long paths must resolve. KNOWN_ISSUES.md item 19: a pathname of 24 bytes or more
# killed VFS — it wrote the REQ_LOOKUP terminator one byte past the 56-byte message
# at exactly 24, and truncated anything longer to 24 and resolved the wrong name.
# The path now travels in a grant. Steps bracket that boundary so a regression names
# the length it stopped at, and each read-back is a marker no other step can print.
# *Creating* a name past 28 bytes is item 20, still open, and is not what this gate
# measures. x86 only, like `test-coreutils-wedge`; the other arches have images and
# could run the same scenario.
test-long-path boot-timeout="30": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/long-path.tsv sh tools/smoke/feed.sh target/test-long-path.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "long-path: every length resolved"

# Boot the bash that `build-bash` produced and drive `tools/smoke/bash.tsv` at
# it: the version banner, a `-c` command line, an arithmetic expansion, a loop, a
# redirection read back through a second process, an external command (bash's
# fork+exec path) and `$PWD` from its own startup. Nothing else in the tree boots
# bash, and the shared smoke scenario cannot: it types into the minix shell and
# waits for its `#` prompt, which an interactive bash would replace with
# `bash-5.3#`.
#
# The injection is what puts bash in the image: it is deliberately not a
# `BOOT_BINS` entry, so an image build never depends on bash having been built.
test-bash arch="x86" boot-timeout="60": (build-bash arch)
    @just test-bash-{{arch}} {{boot-timeout}}

test-bash-x86 boot-timeout="40":
    MINIXFS_EXTRA='/bin/bash=target/bash/x86/bash' just build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/bash.tsv sh tools/smoke/feed.sh target/test-bash-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "bash: every step of tools/smoke/bash.tsv answered (x86_64)"

test-bash-riscv64 boot-timeout="60":
    MINIXFS_EXTRA='/bin/bash=target/bash/riscv64/bash' just build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/bash.tsv sh tools/smoke/feed.sh target/test-bash-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "bash: every step of tools/smoke/bash.tsv answered (riscv64)"

test-bash-aarch64 boot-timeout="60":
    MINIXFS_EXTRA='/bin/bash=target/bash/aarch64/bash' just build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/bash.tsv sh tools/smoke/feed.sh target/test-bash-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "bash: every step of tools/smoke/bash.tsv answered (aarch64)"

# Boot the dynamic-linking artifacts and drive `tools/smoke/dyn.tsv` at them.
# `/bin/dynhello` is an `ET_EXEC` with `PT_INTERP /libexec/ld.so` and `DT_NEEDED`
# for two shared objects, one of which names the other, so the strings that live
# only in those objects are what prove the loader chose a base for each of them,
# followed a dependency of a dependency, resolved names across objects, applied
# each object's own `RELATIVE` fixups and `R_X86_64_COPY`, and ran the objects'
# initialisers in dependency order. Two further steps ask for a fork after the
# load (the child calls into both objects) and an exec after it (the replacement
# program loads again).
#
# The negative check is the centre of this gate: no `dynlink` string may be in
# `/bin/dynhello`. Were one there, the program could print it with no loader at
# all and the boot below would pass while proving nothing. (The check is against
# the built program, which is the file `/bin/dynhello` is a copy of.)
#
# The injection is what puts the four files in the image: like bash, they are
# deliberately not `BOOT_BINS`, so an image build never depends on them having
# been built.
test-dynlink-x86 boot-timeout="40": dynlink-x86
    DYNLINK_BINS='/libexec/ld.so=target/x86_64-pc-minix/release/ldso;/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/bin/dynhello=target/dynlink/x86/dynhello;/lib/libc.so=target/dynlink/x86/libc.so;/bin/dynclib=target/dynlink/x86/dynclib' just build-x86
    @if grep -q 'dynlink' target/dynlink/x86/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/dynlink/x86/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (x86_64)"

# The same artifacts for riscv64. `tools/build-dynlink.py` and
# `tools/build-dynlibc.py` take the target; only the loader's `_start` and its TLS
# placement differ between them (`crates/ldso`).
dynlink-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target riscv64gc-unknown-minix --release
    python tools/build-dynlink.py riscv64
    python tools/build-dynlibc.py riscv64

# The dynamic-linking gate for riscv64: the same `tools/smoke/dyn.tsv` scenario as
# x86_64, which is deliberately arch-neutral — it asserts on symbols only the shared
# objects contain, on an errno read through the loader's TLS, and on the program's
# own constructor, none of which depends on the instruction set.
test-dynlink-riscv64 boot-timeout="60": dynlink-riscv64
    DYNLINK_BINS='/libexec/ld.so=target/riscv64gc-unknown-minix/release/ldso;/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/bin/dynhello=target/dynlink/riscv64/dynhello;/lib/libc.so=target/dynlink/riscv64/libc.so;/bin/dynclib=target/dynlink/riscv64/dynclib' just build-riscv64
    @if grep -q 'dynlink' target/dynlink/riscv64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/dynlink/riscv64/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (riscv64)"

dynlink-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target aarch64-unknown-minix --release
    python tools/build-dynlink.py aarch64
    python tools/build-dynlibc.py aarch64

test-dynlink-aarch64 boot-timeout="60": dynlink-aarch64
    DYNLINK_BINS='/libexec/ld.so=target/aarch64-unknown-minix/release/ldso;/lib/libdyn.so=target/dynlink/aarch64/libdyn.so;/lib/libdyn2.so=target/dynlink/aarch64/libdyn2.so;/bin/dynhello=target/dynlink/aarch64/dynhello;/lib/libc.so=target/dynlink/aarch64/libc.so;/bin/dynclib=target/dynlink/aarch64/dynclib' just build-aarch64
    @if grep -q 'dynlink' target/dynlink/aarch64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/dynlink/aarch64/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (aarch64)"

# Measure whether two processes that map one object through the loader share its
# physical frames — Phase 5 of DYNAMIC_LINKING.md. `tools/dso_share_probe.py` boots
# the dynamic image, runs `/bin/dynclib hold | /bin/dynclib hold` (two lives of the
# same dynamic image, `libc.so` mapped by the loader in each), then walks both
# processes' page tables from outside the guest and compares the frames behind the
# object's read-only pages. The two lives start together, which is the case that needs
# VM to join a fill already in flight; it is a probe rather than a smoke gate because it
# drives QEMU itself, so run it when VM's file fault path or the loader changes.
probe-dso-share-x86: dynlink-x86
    DYNLINK_BINS='/libexec/ld.so=target/x86_64-pc-minix/release/ldso;/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/bin/dynhello=target/dynlink/x86/dynhello;/lib/libc.so=target/dynlink/x86/libc.so;/bin/dynclib=target/dynlink/x86/dynclib' just build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/dso_share_probe.py

image-riscv64 boot-timeout="15": build-riscv64
    mkdir -p target/images/riscv64gc-unknown-minix
    cp target/riscv64gc-unknown-minix/release/kernel-boot-riscv64 target/images/riscv64gc-unknown-minix/minix-riscv64.elf
    @just _assert-qemu-version qemu-system-riscv64
    sh tools/smoke/feed.sh target/image-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64.elf
    @just _assert-qemu-log target/image-riscv64.log "wserver: ready"
    @echo "done: target/images/riscv64gc-unknown-minix/minix-riscv64.elf — qemu-system-riscv64 -machine virt -m 256M -nographic -kernel <it>"

image-aarch64 boot-timeout="15": build-aarch64
    mkdir -p target/images/aarch64-unknown-minix
    cp target/aarch64-unknown-minix/release/kernel-boot-aarch64 target/images/aarch64-unknown-minix/minix-aarch64.elf
    @just _assert-qemu-version qemu-system-aarch64
    sh tools/smoke/feed.sh target/image-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64.elf
    @just _assert-qemu-log target/image-aarch64.log "wserver: ready"
    @echo "done: target/images/aarch64-unknown-minix/minix-aarch64.elf — qemu-system-aarch64 -machine virt -m 256M -nographic -kernel <it>"

# ---------- desktop (graphical window; shell stays on stdio) ----------
# The SDL window shows the framebuffer and routes host keys to the guest
# keyboard (PS/2 on x86, virtio-keyboard on riscv/aarch64), so the
# wserver desktop is interactive: click the window and type into the
# focused wdemo window while the shell runs on the terminal. The pointer
# is a virtio-tablet (absolute): the guest cursor tracks the host cursor
# 1:1 and works whether or not the SDL grab is held — with a relative
# virtio-mouse, releasing the grab (Alt+Ctrl+G or leaving the window)
# stops guest motion until a re-grab takes. The display gets an id
# because QEMU resolves `display=` (and QMP input-send-event device
# names) by id.

desktop target="x86" memory="256M":
    @just desktop-{{target}} {{memory}}

desktop-x86 memory="256M": build-x86 mkfs-x86
    qemu-system-x86_64 -display sdl -serial stdio -m {{memory}} -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0

desktop-riscv64 memory="256M": build-riscv64 mkfs-riscv64
    qemu-system-riscv64 -machine virt -m {{memory}} -display sdl -serial stdio -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64

desktop-aarch64 memory="256M": build-aarch64 mkfs-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m {{memory}} -display sdl -serial stdio -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- debug (QEMU gdb stub on :1234) ----------

debug target="x86":
    @just debug-{{target}}

debug-x86: build-x86 mkfs-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -s -S -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0

debug-aarch64: build-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -display none -serial stdio -no-reboot -s -S -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- tests ----------

# RISC-V/AArch64 test builds (fork stage1 compiler, in-tree targets). The
# integration-tests feature runs kernel::tests::run_all() in QEMU before any
# userspace starts; boot-test runs the multi-server boot suite after VFS
# mount_root.
#
# The suites self-terminate (isa-debug-exit on x86, SBI SRST / PSCI elsewhere),
# so every QEMU run is bounded: a guest that hangs has to fail the recipe with its
# serial tail from `_assert-qemu-log`, not walk the CI job into its own timeout.
# `/usr/bin/timeout` is spelled out for the same reason the image recipes do it - a
# bare `timeout` is the Windows one, whose command line is unrelated.
#
# Seconds one QEMU run may take before it is killed. A passing suite is seconds of
# guest time once the build is out of the way, so this is bulk headroom for a slow
# or loaded emulator, not a performance budget.
qemu-timeout := "60"

# The oldest emulator the suites pass on. An older one is not a slow-but-working
# case to wait out: on the QEMU 8.2 that ubuntu-24.04 ships, the aarch64 boot suite
# hangs forever at "scheduler starting..." behind a permanent IRQ storm, so a stale
# emulator only looks like a timeout. Fail with its version instead.
qemu-min-version := "11"

# Refuse to boot a guest on an emulator older than `qemu-min-version`; `emulator`
# is the qemu-system-* binary the calling recipe is about to run. Reported above
# `_assert-qemu-log`, because a killed run's log tail says nothing about why.
_assert-qemu-version emulator:
    @command -v {{emulator}} > /dev/null || (echo "!! {{emulator}} is not on PATH." >&2; exit 1)
    @v=$({{emulator}} --version 2>/dev/null | sed -n '1s/^QEMU emulator version \([0-9][0-9.]*\).*/\1/p'); major=${v%%.*}; if [ -z "$major" ] || [ "$major" -lt {{qemu-min-version}} ]; then echo "!! {{emulator}} is version ${v:-unknown}; the suites need QEMU {{qemu-min-version}} or newer - older emulators hang the aarch64 boot suite." >&2; exit 1; fi

build-riscv64-test: userland-riscv64 coreutils-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-test --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,integration-tests --release

build-aarch64-test: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64-test --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,integration-tests --release

build-riscv64-boot: userland-riscv64 coreutils-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-boot --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,boot-test --release

build-aarch64-boot: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64-boot --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,boot-test --release

test-qemu target="x86":
    @just test-qemu-{{target}}

test-qemu-x86: build-x86-test mkfs-x86
    @just _assert-qemu-version qemu-system-x86_64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/kernel-test-trampoline.elf -device loader,file=target/kernel-test.bin,addr=0x200000 -device isa-debug-exit -monitor none -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 2>&1 | tee target/test-qemu-x86.log
    @just _assert-qemu-log target/test-qemu-x86.log "-- done --"

# RISC-V exits via SBI SRST (no exit-code device in this QEMU build), so the
# serial log is the gate - see _assert-qemu-log above.
test-qemu-riscv64: build-riscv64-test
    @just _assert-qemu-version qemu-system-riscv64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64-test 2>&1 | tee target/test-qemu-riscv64.log
    @just _assert-qemu-log target/test-qemu-riscv64.log "ALL TESTS PASSED"

# AArch64 exits via PSCI SYSTEM_OFF (always exit code 0 - no exit-code device
# and no semihosting on this QEMU build), so the serial log is the gate.
test-qemu-aarch64: build-aarch64-test
    @just _assert-qemu-version qemu-system-aarch64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64-test 2>&1 | tee target/test-qemu-aarch64.log
    @just _assert-qemu-log target/test-qemu-aarch64.log "ALL TESTS PASSED"

test-boot target="x86":
    @just test-boot-{{target}}

# Gate a QEMU test run on its serial log. The riscv64/aarch64 kernels report no
# exit status through SBI SRST / PSCI on this QEMU build, so the log is the only
# evidence that the suite ran to completion - without this the recipes exit 0
# whatever the guest does. `marker` is the summary the suite prints when every
# check passed (`ALL TESTS PASSED`, or `-- done --` for the x86 kernel suite).
#
# The recipes `tee` the serial output, so the log also holds host-side failures
# (a missing qemu binary, a bad device) and a QEMU that dies mid-run. All of
# those fail here, because the marker never appears.
_assert-qemu-log log marker:
    @if grep -q -- "FAILURES:" {{log}}; then echo "!! failures reported in {{log}}:"; grep -n -- "FAILURES:" {{log}}; exit 1; fi
    @if ! grep -qF -- "{{marker}}" {{log}}; then echo "!! no '{{marker}}' in {{log}} - the suite did not run to completion. Tail:"; tail -30 {{log}}; exit 1; fi

test-boot-x86: build-x86-boot mkfs-x86
    @just _assert-qemu-version qemu-system-x86_64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/kernel-boot-trampoline.elf -device loader,file=target/kernel-boot.bin,addr=0x200000 -device isa-debug-exit -monitor none -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 2>&1 | tee target/test-boot-x86.log
    @just _assert-qemu-log target/test-boot-x86.log "ALL TESTS PASSED"

test-boot-riscv64: build-riscv64-boot mkfs-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64-boot 2>&1 | tee target/test-boot-riscv64.log
    @just _assert-qemu-log target/test-boot-riscv64.log "ALL TESTS PASSED"

test-boot-aarch64: build-aarch64-boot mkfs-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    /usr/bin/timeout -s 9 {{qemu-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64-boot 2>&1 | tee target/test-boot-aarch64.log
    @just _assert-qemu-log target/test-boot-aarch64.log "ALL TESTS PASSED"

# Both QEMU suites for all three arches: the six gates a change to a VA layout, a
# HAL constant or anything arch-gated has to be followed by. They cover different
# things and neither substitutes for the other - `test-qemu` runs the in-kernel
# suite (`crates/kernel/src/tests.rs`, behind the `qemu-tests` feature), `test-boot`
# boots to a shell and runs `boot_test.rs` - and the host suite cannot stand in for
# the first, because `tests.rs` is compiled only under `qemu-tests`. That is
# exactly how `syscall_brk` came to assert x86_64's heap base and fail on aarch64
# alone: `cargo test` never saw it.
#
# Grouped by arch so each arch's userland/coreutils build is reused, and stops at
# the first failing gate like the individual recipes (the failing log is on
# stdout).
#
# All six QEMU gates in one command.
test-arches:
    @just test-qemu x86
    @just test-boot x86
    @just test-qemu riscv64
    @just test-boot riscv64
    @just test-qemu aarch64
    @just test-boot aarch64

test target="x86":
    @just test-{{target}}

test-riscv64: build-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64

test-kernel target="x86":
    @just test-qemu {{target}}

# Write the root filesystem blob to the per-arch
# target/images/<triple>/disk.img for the virtio-blk drive (kept separate
# per arch so one arch's mkfs never clobbers another's disk image).
# The `rm -f` clears any stale output first: MSVC's link.exe fails LNK1104
# when the target exe exists and is momentarily locked (antivirus scan / a
# lingering run).
mkfs target="x86":
    @just mkfs-{{target}}

mkfs-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    rm -f target/mkfs target/mkfs.exe
    "{{stage1-rustc}}" tools/mkfs.rs --edition 2021 -o target/mkfs
    target/mkfs x86_64

mkfs-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    rm -f target/mkfs target/mkfs.exe
    "{{stage1-rustc}}" tools/mkfs.rs --edition 2021 -o target/mkfs
    target/mkfs riscv64

mkfs-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    rm -f target/mkfs target/mkfs.exe
    "{{stage1-rustc}}" tools/mkfs.rs --edition 2021 -o target/mkfs
    target/mkfs aarch64

# Rebuild the C smoke-test binaries (/bin/helloc, /bin/ctest) from tools/hello.c,
# tools/ctest.c and the arch's tools/crt0-<arch>.S (clang freestanding +
# minix-libc, linked with the fork rustc). For x86 it then re-embeds them in the
# initramfs and disk image, which needs `just build-x86` once so target/mkboot
# exists; the other arches embed through their own `just build-<arch>`
# (`crates/kernel/build.rs`).
build-c-hello arch="x86":
    #!/bin/sh
    set -eu
    test -n "{{stage1-rustc}}" || { echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2; exit 1; }
    python tools/build-c-hello.py {{arch}}
    # The re-embed is the x86 images' (target/mkboot assembles them, and needs one
    # `just build-x86` first); the other arches embed through their own
    # `just build-<arch>` — crates/kernel/build.rs.
    if [ "{{arch}}" != x86 ]; then exit 0; fi
    test -x target/mkboot || { echo 'error: target/mkboot missing — run `just build-x86` once' >&2; exit 1; }
    target/mkboot embed_initramfs,embed_minixfs
    rm -f target/mkfs target/mkfs.exe
    "{{stage1-rustc}}" tools/mkfs.rs --edition 2021 -o target/mkfs
    target/mkfs x86_64

# GNU bash for a minix target, from the upstream commit `tools/build-bash.py` pins:
# fetched, configured against `tools/c-include` and linked against minix-libc by
# `tools/cc-minix.py`, the `cc` a C project's own build needs. That host must be
# POSIX (the stage1, the rlib and clang all have to be the same host's), so on
# Windows the script re-enters WSL by itself — `MINIX_WSL_DISTRO` picks the
# distribution. Artifact: `target/bash/<arch>/bash`; `just test-bash <arch>`
# boots it. `C_BUILD.md` has the why of each flag and each trap this build costs.
#
# Run this *after* the toolchain: `bootstrap` and `fetch-stage1` start with
# `cargo clean`, which takes `target/bash-src` and the build tree with it.
build-bash arch="x86":
    python tools/build-bash.py {{arch}}

# The dynamic-linking artifacts (x86_64 only): the loader (`/libexec/ld.so`,
# `crates/ldso` linked with its own script, `tools/minix-ldso.ld`), and, through
# `tools/build-dynlink.py`, the two shared objects and the dynamically linked test
# program. Phase 3 adds, through `tools/build-dynlibc.py`, `libc.so` itself and a
# C program linked against it. `test-dynlink-x86` puts all six in an image and
# boots it.
dynlink-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found - run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target x86_64-pc-minix --release
    python tools/build-dynlink.py x86
    python tools/build-dynlibc.py x86

# Build the C++ runtime (libc++ + libc++abi) for the x86_64 Minix cross
# toolchain and merge them into target/cxx/minix-runtime/libstdc++.a.
#
# Freestanding CMake cross builds (host clang targeting x86_64-unknown-none)
# against the Minix C headers (tools/c-include); requires
# target/cxx/toolchain-x86_64.cmake. Quirks handled here:
#   - both libs need a distinct *_SHARED_OUTPUT_NAME: the never-built shared
#     target collides with the static archive name on the Generic platform
#     (ninja "multiple rules generate lib/libc++.a")
#   - libc++ needs LIBCXX_ENABLE_THREADS=OFF (_LIBCPP_HAS_NO_THREADS);
#     libc++abi keeps RTTI (private_typeinfo.cpp uses dynamic_cast), so it
#     uses the plain toolchain, not the -fno-rtti LLVM one
#   - include order must be libc++ -> c-include -> libcxx-build config, or
#     libc++'s <string.h> guard skips the C headers and ::size_t is missing
#   - the IWYU mapping step needs Python3_EXECUTABLE; the standalone libcxx
#     cmake never sets it, and the host's only python (the embeddable CPython
#     beside LLVM, whose python311._pth pins sys.path) cannot import libcxx's
#     local modules, so tools/libcxx-iwyu.cmd stubs the step
libcxx-x86:
    rm -rf target/cxx/libcxx-build
    cmake -G Ninja -S rust/src/llvm-project/libcxx -B target/cxx/libcxx-build -DCMAKE_TOOLCHAIN_FILE={{ROOT}}/target/cxx/toolchain-x86_64.cmake -DCMAKE_BUILD_TYPE=Release -DLIBCXX_ENABLE_SHARED=OFF -DLIBCXX_ENABLE_STATIC=ON -DLIBCXX_ENABLE_EXCEPTIONS=OFF -DLIBCXX_ENABLE_RTTI=OFF -DLIBCXX_ENABLE_FILESYSTEM=OFF -DLIBCXX_ENABLE_LOCALIZATION=ON -DLIBCXX_ENABLE_MONOTONIC_CLOCK=ON -DLIBCXX_ENABLE_NEW_DELETE_DEFINITIONS=OFF -DLIBCXX_ENABLE_RANDOM_DEVICE=ON -DLIBCXX_ENABLE_ABI_LINKER_SCRIPT=OFF -DLIBCXX_ENABLE_THREADS=OFF -DLIBCXX_ABI_VERSION=1 -DLIBCXX_ABI_NAMESPACE=__1 -DLIBCXX_CXX_ABI=libcxxabi -DLIBCXX_AVAILABILITY_MINIMUM_HEADER_VERSION=2 -DLIBCXX_SHARED_OUTPUT_NAME=cxx-shared -DLIBCXX_INCLUDE_TESTS=OFF "-DLIBCXX_ADDITIONAL_COMPILE_FLAGS=-I{{ROOT}}/rust/src/llvm-project/libcxxabi/include;-D_POSIX_TIMERS=200809L" "-DPython3_EXECUTABLE={{ROOT}}/tools/libcxx-iwyu.cmd"
    ninja -C target/cxx/libcxx-build
    rm -rf target/cxx/libcxxabi-build
    cmake -G Ninja -S rust/src/llvm-project/libcxxabi -B target/cxx/libcxxabi-build -DCMAKE_TOOLCHAIN_FILE={{ROOT}}/target/cxx/toolchain-x86_64.cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_FLAGS="-ffreestanding -fno-pic -mno-red-zone -fno-stack-protector -O2 -I{{ROOT}}/tools/c-include" -DCMAKE_CXX_FLAGS="-ffreestanding -fno-pic -mno-red-zone -fno-stack-protector -O2 -std=c++23 -nostdinc++ -I{{ROOT}}/rust/src/llvm-project/libcxx/include -I{{ROOT}}/tools/c-include -I{{ROOT}}/target/cxx/libcxx-build/include/c++/v1" -DLIBCXXABI_ENABLE_SHARED=OFF -DLIBCXXABI_ENABLE_STATIC=ON -DLIBCXXABI_BAREMETAL=ON -DLIBCXXABI_ENABLE_THREADS=OFF -DLIBCXXABI_ENABLE_EXCEPTIONS=OFF -DLIBCXXABI_ENABLE_NEW_DELETE_DEFINITIONS=ON -DLIBCXXABI_AVAILABILITY_MINIMUM_HEADER_VERSION=2 -DLIBCXXABI_INCLUDE_TESTS=OFF -DLIBCXXABI_USE_LLVM_UNWINDER=OFF -DLIBCXXABI_ENABLE_STATIC_UNWINDER=OFF -DLIBCXXABI_SHARED_OUTPUT_NAME=cxxabi-shared
    ninja -C target/cxx/libcxxabi-build
    rm -rf target/cxx/merge-tmp
    mkdir -p target/cxx/merge-tmp
    cd target/cxx/merge-tmp && llvm-ar x ../libcxxabi-build/lib/libc++abi.a && llvm-ar x ../libcxx-build/lib/libc++.a && mkdir -p ../minix-runtime && llvm-ar rcs ../minix-runtime/libstdc++.a *.obj && cd .. && rm -rf merge-tmp

# ---------- check ----------

# Run the host test suite (and clippy) on Linux, in a container on the podman
# WSL machine — the same gate CI's `host-tests` job runs on ubuntu-latest.
#
# A Windows-only host run cannot see host-libc portability bugs; this recipe
# is how CI's Linux failures get reproduced locally (see the
# `linux-host-tests` skill). The repo is mounted **read-only** and all build
# output stays inside the container, so the working tree is never touched.
#
# Needs: podman with a `podman machine` VM, and network for the image pull.
test-linux:
    @podman machine start >/dev/null 2>&1 || true
    MSYS_NO_PATHCONV=1 podman run --rm --network=host -e RUSTUP_TOOLCHAIN={{rust-channel}}-x86_64-unknown-linux-gnu -e CARGO_TARGET_DIR=/tmp/just-target -v "{{ROOT}}:/w:ro" -w /w rust:{{rust-channel}} bash -c 'export PATH=/usr/local/cargo/bin:/usr/bin:/bin; cargo test --workspace --no-fail-fast --locked 2>&1 | tee /tmp/t.log; t=${PIPESTATUS[0]}; grep -h "^test result:" /tmp/t.log | awk "{p+=\$4;i+=\$8} END {printf \"linux host tests: passed=%d ignored=%d summaries=%d\\n\",p,i,NR}"; if [ $t -ne 0 ]; then echo "--- failures ---"; grep -nE "test result: FAILED|^error|signal:|panicked" /tmp/t.log | head -20; fi; rustup component add clippy >/dev/null 2>&1; cargo clippy --all-targets -- -D warnings; c=$?; echo "linux host tests: cargo-test rc=$t clippy rc=$c"; test $t -eq 0 -a $c -eq 0'

# Host clippy, riscv64 compilation check (fork stage1 compiler), and the C
# header contract: the headers in tools/c-include are what the C and C++
# consumers compile against (bash, /bin/helloc and /bin/ctest, the libc++
# build), and nothing else notices when an export and its declaration drift
# apart. The checker parses the Rust sources, so it needs no cbindgen.
check:
    cargo clippy -- -D warnings
    python tools/check-c-headers.py
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" cargo check -p kernel-boot --bin kernel-boot-riscv64 --features riscv64 --target riscv64gc-unknown-minix --release

# Build the wasm system and stage the demo into `docs/`, the directory GitHub Pages serves.
#
# That is the whole publishing story for the browser demo: no CI, no server, no account — a
# branch and a folder. What it costs is that the assets have to be *in* the repository, which is
# why `.gitignore` keeps `*.wasm` out everywhere except `docs/build/`, and why the boot image
# (16 MiB) goes in with them: expect ~19 MiB per published build, so rebuild it when the demo
# should change rather than on every commit.
#
# It ends by booting the staged copy (`tools/wasm-browser/publish-check.mjs`), which is what
# makes the published page the *tested* page instead of a copy of it.
publish-wasm:
    sh tools/wasm-browser/publish.sh

# Remove generated assets that must be rebuilt from scratch.
clean:
    rm -rf target/nested target/images
    rm -f target/initramfs.cpio target/minixfs.img target/trampoline.elf target/trampoline_.o target/kernel.bin target/mkboot target/mkboot.exe
