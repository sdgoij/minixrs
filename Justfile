# Build and run the MINIX/Rust port in QEMU.
#
# Targets: x86 (default), riscv64, aarch64.
#
# Requires:
#   - `just bootstrap` first (builds the rust fork stage1 compiler, which
#     provides the in-tree minix targets + their std sysroot)
#   - QEMU 11 or newer (the gate refuses an older emulator) and clang on PATH —
#     clang assembles the x86 trampoline
#     and the C/C++ smoke tests; the toolchain's own tools (lld, nm, objcopy) are
#     resolved from its build tree, with PATH only as a fallback (see `minix-lld`
#     below)
#
# The recipes orchestrate plain `cargo` invocations. The kernel-embedded image
# (initramfs CPIO + MinixFS) is assembled by `crates/kernel/build.rs`; the *disk*
# system image is assembled by `boot-image`'s `mkminixfs`, which reads
# `MINIXFS_EXTRA` / `DYNLINK_BINS` / `MINIXFS_BLOCKS`. x86 post-link work
# (trampoline + kernel.bin) lives in `cargo xtask image mkboot`.

# Path to the rust fork's stage1 rustc (built by `just bootstrap`); used as
# the RUSTC for userland/server builds so the in-tree minix targets and
# their std sysroot are used (no `-Zbuild-std`, no JSON specs). `tools/stage1.py`
# owns the lookup, because a checkout shared between Windows and WSL holds one
# stage1 per host and only this host's runs here — see its docstring.
stage1-rustc := `python tools/stage1.py`

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
# `build` -> `build-x86`, `system-image` -> `system-image-x86`, ...) have the value converted
# when the inner `just.exe` starts, before any line of the inner recipe runs.
# The list is semicolon-separated; `DYNLINK_BINS` has the same hazard as
# `MINIXFS_EXTRA` (`/libexec/ld.so=<path>;...` is a POSIX-style value too).
export MSYS2_ENV_CONV_EXCL := "MINIXFS_EXTRA;DYNLINK_BINS;MINIXFS_ROOT"

# Repo root with forward slashes (the recipe shell mangles backslashes).
ROOT := replace(justfile_directory(), "\\", "/")

# Rust channel pinned by rust-toolchain.toml; also the tag of the container
# image `check-linux` uses, so the two cannot drift apart.
rust-channel := `sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml | tr -d '\r'`

# Fetches the build-critical submodules (the rust fork, the uutils coreutils
# source and the libc fork) when they're missing — `git submodule update
# --init` is a no-op when they're already present and at the pinned commits.
# `libc` is required rather than convenient: `rust/library` and `coreutils` both
# `[patch.crates-io]` it to the sibling fork, so cargo cannot resolve either
# workspace without it. Regenerates `rust/config.toml` via
# `cargo xtask rust-config` for the requested arch, then runs x.py and builds the
# `/bin/hello` std smoke-test binary with `cargo xtask build std-hello`.
# Note: per-arch runs rebuild only that arch's std — x.py prunes the other
# arches from the stage1 sysroot — so `all` (the default) is the complete
# setup. Incremental afterwards; the first run downloads the stage0
# toolchain and CI LLVM (needs network).
# Build the stage1 compiler + std + /bin/hello for a minix target (all by default).
bootstrap target="all":
    git submodule update --init rust coreutils libc
    cargo xtask rust-config {{target}}
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
    cargo xtask build std-hello {{target}}
    # The C smoke-test binaries (helloc/ctest) also live under target/ and are
    # wiped by the clean. They build for all three arches now
    # (`just build-c-hello <arch>`), but the boot images embed them on x86_64
    # only (`crates/kernel/build.rs`), so only that one is rebuilt here.
    if [ "{{target}}" = x86 -o "{{target}}" = all ]; then cargo xtask build c-hello x86; fi

# Install the prebuilt stage1 toolchain for the commit the `rust` submodule
# pins, fetched from a release on the rust fork and checksum-verified, instead
# of building LLVM and the compiler from source. It is the same toolchain
# `bootstrap all` produces (host + all three minix targets) and is therefore
# host-specific; `bootstrap` stays authoritative for work inside the fork.
# Fetch the prebuilt stage1 toolchain instead of building it from source (`bootstrap`).
# A published release can be verified by consuming it: `just verify-stage1`
# (Linux/WSL only - the asset is host-specific).
fetch-stage1 target="all":
    cargo xtask fetch-stage1
    @just _finish-bootstrap {{target}}
    @echo "prebuilt stage1 ready. Rebuild the images: just build-{{ if target == "all" { "x86" } else { target } }} && just mkfs-{{ if target == "all" { "x86" } else { target } }}; boot with just run-{{ if target == "all" { "x86" } else { target } }} 256M"

# Consume the published stage1 the way a Linux dev or CI job does: fetch it (into
# target/stage1-verify), link the smoke binaries for all three minix targets with
# it, and check each result's ELF machine. With the published asset that is a
# Linux run; a stage1 built from source can be verified on the host that built
# it by naming its directory and its lld (`MINIXRS_STAGE1_DEST`, `MINIXRS_LLD`).
verify-stage1:
    cargo xtask verify-stage1

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
    # port's own, `cargo xtask cc minix aarch64`, with an archiver for the object:
    # `llvm-ar` where it exists (MSYS has no `ar`), binutils' `ar` otherwise (a Linux
    # runner). Both are named bare, so cc-rs resolves them on PATH.
    #
    # The wrapper is named by path, not through `cargo run`: cc-rs calls it once per C
    # file, and a `cargo run` per call would queue behind cargo's lock. The first line
    # builds it; `cargo xtask --list` prints the command tree, which is discarded.
    cargo xtask --list >/dev/null
    AR_aarch64_unknown_minix="$(command -v llvm-ar >/dev/null 2>&1 && echo llvm-ar || echo ar)" CC_aarch64_unknown_minix="{{ROOT}}/.xtask/debug/xtask cc minix aarch64" RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-T../tools/minix-user.ld -C link-arg=--no-eh-frame-hdr --cfg getrandom_backend=\"custom\" -C strip=symbols -C opt-level=z" cargo build --manifest-path coreutils/Cargo.toml --release --target aarch64-unknown-minix --no-default-features --features feat_minix
    cp coreutils/target/aarch64-unknown-minix/release/coreutils target/aarch64-unknown-minix/release/coreutils

# ---------- build ----------

build target="x86":
    @just build-{{target}}

# build-x86* embeds /bin/coreutils, so the multicall must be built and copied
# into the shared target dir first.
#
# The embedded root gets 5120 zones (20 MiB) rather than `crates/boot-image`'s 16 MiB
# default: the x86 userland fills 4082 of those 4096 zones once `/bin/bash` is injected, so
# at the default the builder refuses the image for want of a free zone. `/bin/bash` reaches
# this recipe through `MINIXFS_EXTRA`, named by the `image` gate (the release
# artifact) and `prepare-bash-<arch>`. 32 MiB, which `prepare-image-riscv64` uses, is not available here: the boot code
# copies the embedded root into physical RAM for the ramdisk server (`boot_init.rs`), so
# every zone added costs the guest a byte of its 256 MiB, and 8192 runs out of memory for a
# brk heap at `-m 256M`. A caller with a larger payload still overrides `MINIXFS_BLOCKS`.
build-x86: userland-x86 coreutils-x86 dynlib-x86
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-5120}" cargo xtask image mkboot embed_initramfs,embed_minixfs

build-x86-test: userland-x86 coreutils-x86 dynlib-x86
    cargo xtask image mkboot embed_initramfs,embed_minixfs,integration-tests kernel-test

build-x86-boot: userland-x86 coreutils-x86 dynlib-x86
    cargo xtask image mkboot embed_initramfs,embed_minixfs,boot-test kernel-boot

# The boot image: kernel + initramfs only. This is what a disk-root boot needs, so
# the kernel carries no root filesystem and is not capped by the 16 MiB ramdisk
# window (`RAMDISK_IMAGE_SIZE`; an embedded root past it lands on the user mmap
# region at 4 GiB). Output: target/boot-kernel.bin + target/boot-kernel-trampoline.elf,
# mirrored to target/images/<triple>/minix-x86-boot.elf for the release.
boot-image-x86: userland-x86 coreutils-x86 dynlib-x86
    cargo xtask image mkboot embed_initramfs boot-kernel
    mkdir -p target/images/x86_64-pc-minix
    cp target/boot-kernel-trampoline.elf target/images/x86_64-pc-minix/minix-x86-boot.elf

# The system image: the full root filesystem on a raw disk for virtio-blk. coreutils
# is part of it, and so is anything a caller injects (`MINIXFS_EXTRA`, merged ahead of
# the caller's own value; the Mesa DSOs are the ~36 MiB case, `.notes/wayland.md` §6.10 stage
# 3c-2). GNU bash is included when its artifact is present — `just build-bash <arch>`
# locally, or the release's downloaded `bash-<arch>` — and its heavy C build is never
# forced onto the `run-<arch>` path. The image builder reads `MINIXFS_EXTRA` /
# `DYNLINK_BINS` / `MINIXFS_BLOCKS` itself, so a root larger than the 16 MiB embedded
# window needs no kernel change.
#
# The staged toolchain root follows the same rule as `/bin/bash`: include it if
# the artifact is there, say so and skip it if not. `toolchain-root-x86` is the
# explicit builder (`just llvm-minix x86`), not a dependency of this recipe; add
# `rustc-root-x86` to that root and it also carries the in-guest rustc. The root
# with the C/C++ toolchain needs 98304 blocks and 16384 inodes; with rustc it is
# about 610 MiB, so the image is 768 MiB when it is included and the
# release-sized image otherwise.
# The in-guest Rust compiler: `crates/rustc-minix` cross-built and staged into
# the toolchain root with the target's sysroot. Additive to `toolchain-root-x86`,
# which the gate prepares first.
rustc-root-x86: toolchain-root-x86
    @cargo xtask build rustc-minix x86

toolchain-root-x86:
    @just llvm-minix x86

system-image-x86: boot-image-x86
    extras=""; if [ -f target/bash/x86/bash ]; then extras=/bin/bash=target/bash/x86/bash; else echo "system-image: no /bin/bash (run 'just build-bash x86' to include it)"; fi; \
    if [ -d target/toolchain/x86/root ]; then \
        echo "system-image: including the toolchain"; \
        MINIXFS_ROOT=target/toolchain/x86/root MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-196608}" MINIXFS_INODES="${MINIXFS_INODES:-16384}" cargo run -q -p boot-image --bin mkminixfs --release -- x86_64; \
    else \
        echo "system-image: no toolchain root (run 'just llvm-minix x86' to include it)"; \
        MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- x86_64; \
    fi
    @just mkfs-x86


# riscv64 / aarch64, the same split without mkboot: the kernel ELF *is* the artifact
# (`-kernel`). The boot image is copied to a distinct name so it does not collide with
# the self-contained `release/kernel-boot-<arch>` that the tests and the `image` gate boot.
boot-image-riscv64: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,riscv64 --release
    cp target/riscv64gc-unknown-minix/release/kernel-boot-riscv64 target/boot-kernel-riscv64.elf
    mkdir -p target/images/riscv64gc-unknown-minix
    cp target/boot-kernel-riscv64.elf target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf

system-image-riscv64: boot-image-riscv64
    extras=""; if [ -f target/bash/riscv64/bash ]; then extras=/bin/bash=target/bash/riscv64/bash; else echo "system-image: no /bin/bash (run 'just build-bash riscv64' to include it)"; fi; MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- riscv64
    @just mkfs-riscv64

boot-image-aarch64: userland-aarch64 coreutils-aarch64 dynlib-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,aarch64 --release
    cp target/aarch64-unknown-minix/release/kernel-boot-aarch64 target/boot-kernel-aarch64.elf
    mkdir -p target/images/aarch64-unknown-minix
    cp target/boot-kernel-aarch64.elf target/images/aarch64-unknown-minix/minix-aarch64-boot.elf

system-image-aarch64: boot-image-aarch64
    extras=""; if [ -f target/bash/aarch64/bash ]; then extras=/bin/bash=target/bash/aarch64/bash; else echo "system-image: no /bin/bash (run 'just build-bash aarch64' to include it)"; fi; MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- aarch64
    @just mkfs-aarch64

# Boot the split pair: the minimal boot image plus the disk filesystem.
system-image target="x86":
    @just system-image-{{target}}

build-riscv64: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64 --release

build-aarch64: userland-aarch64 coreutils-aarch64 dynlib-aarch64
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64 --release

# ---------- run ----------

# The system image, booted by the minimal boot image. It includes the staged
# toolchain if `target/toolchain/<arch>/root` is there, the same way it includes
# `/bin/bash`, so `just llvm-minix x86` then `just run x86 2048M` reaches
# `/usr/bin/clang`, `clang++` and `ld.lld`. Without the root it says so and boots
# the release-sized system. `cargo xtask gate image` stays the self-contained 16 MiB system.
run target="x86" memory="256M":
    @just run-{{target}} {{memory}}

run-x86 memory: boot-image-x86 system-image-x86
    qemu-system-x86_64 -nographic -m {{memory}} -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/boot-kernel-trampoline.elf -device loader,file=target/boot-kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0

run-riscv64 memory: boot-image-riscv64 system-image-riscv64
    qemu-system-riscv64 -machine virt -m {{memory}} -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/boot-kernel-riscv64.elf

run-aarch64 memory: boot-image-aarch64 system-image-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m {{memory}} -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/boot-kernel-aarch64.elf

# The 3D half of 3a's gate. x86 keeps bochs-display for the output, so the virtio-gpu
# device the host provides is a render node only (D3): the probe negotiates
# VIRTIO_GPU_F_VIRGL, reads the virgl capset with GET_CAPSET_INFO, fetches the capset
# blob with GET_CAPSET, creates a context, round-trips a 64x64 texture through it two
# ways (RESOURCE_CREATE_3D, ATTACH_BACKING, CTX_ATTACH_RESOURCE, a transfer each way,
# a SUBMIT_3D inline write, RESOURCE_UNREF), destroys the context, and reports all of
# it in one boot line (asserted below).
#
# `xfer` and `submit` are the two fields that are not a command's answer, and they are
# byte counts because they have to be: the host answers NODATA to TRANSFER_TO_HOST_3D,
# TRANSFER_FROM_HOST_3D and SUBMIT_3D even when the renderer refused them, because QEMU
# discards its return value - so a command that did nothing and one that worked are
# identical on the control queue. Each half uploads a position-dependent pattern,
# scribbles over the buffer, reads back, and reports how many leading bytes matched: the
# whole texture is a round trip, anything less is not. Without the scribble a host that
# ignored both transfers would pass, because the resource is guest-backed and the
# pattern would still be sitting in the buffer.
#
# `submit` is the stricter of the two: the command names one row of the texture, so the
# read-back also fails if the host wrote past that row - the whole texture has to come
# back as that row changed and nothing else.
#
# The device's `blob=` property is left off (its default): 3a issues no blob command,
# and asking for the blob feature adds a shared-memory BAR the boot-time identity map
# would have to cover.
#
# A GL device needs a GL display backend, so this recipe cannot use -nographic:
# `-display egl-headless -serial stdio` keeps the console on stdio, which is what
# the smoke driver drives.
#
# Four things had to be true for that line to appear, each found by a bisecting boot,
# and each worth checking first when this goes red again:
#   * the fb process needs the GPU's PCI BARs in its page table (the kernel pre-maps
#     them for blk/net/input only);
#   * the command and response buffers must live in this process's *image*, not in the
#     probe's stack frame, or the host cannot translate the descriptor address and
#     stops the device with `bogus descriptor or out of resources`;
#   * `RESP_OK_DISPLAY_INFO` (0x1101) is a member of the success response enum, so
#     leaving it out shifted the two capset answers down and made the driver reject a
#     correct `GET_CAPSET_INFO` answer;
#   * the round-trip resource has to be a *texture* with a texture bind. A resource
#     whose storage is nothing but guest memory transfers to itself, so its round trip
#     would pass while moving no data at all.
#
# The *degrade* half is graded on every plain x86 boot: the `image` gate asserts
# `fb: gpu3d none`, and the `gpu3d-nogl` gate covers the other degrade case, a device
# the host gave no GL to.
#
# Nothing is typed into this guest, a departure from every other image recipe and a
# deliberate one. Host GL keeps the emulator busy enough that its serial input drops
# bytes - measured: the run that made this change sent `/bin/echo gpu3d-ok` and the
# guest echoed `/bin/echo gpu3d` - and the more the probe asks the renderer to do, the
# wider that window gets, so a typed step here would be a flake waiting to happen. Nor
# is one needed: the assertion is a boot line the fb server prints, and `wserver:
# ready` / `wlserver: ready` are asserted with it so the line cannot come from a
# half-booted guest. QEMU is bounded by `timeout` rather than by a scenario's last
# step, so the recipe's `boot-timeout` is the guest's whole lifetime.

# The same artifacts for riscv64. `cargo xtask build dynlink` and
# `cargo xtask build dynlibc` take the target; only the loader's `_start` and its TLS
# placement differ between them (`crates/ldso`).
dynlib-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target riscv64gc-unknown-minix --release
    cargo xtask build dynlibc riscv64

dynlink-riscv64: dynlib-riscv64
    cargo xtask build dynlink riscv64

dynlib-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target aarch64-unknown-minix --release
    cargo xtask build dynlibc aarch64

dynlink-aarch64: dynlib-aarch64
    cargo xtask build dynlink aarch64

# gltriangle: build the surfaceless-EGL GLES2 triangle client and stage the Mesa
# DSOs it runs against (§6.10 stage 3c-2) for one target.
#
# The client needs ~36 MiB of DSOs — two copies of the 17 MiB `libgallium`
# (`libEGL`/`libGLESv2` name it in `DT_NEEDED`, and EGL *also* `dlopen`s it as
# `swrast_dri.so`) — which is why they live in the *system image*, never the kernel:
# a boot image that embedded them would exceed the 16 MiB ramdisk window. Build them
# with `just build-mesa <arch> --build` and `just libcxx <arch>` first.
gltriangle arch="x86":
    @just gltriangle-{{arch}}

gltriangle-x86: dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build gltriangle x86
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (x86_64)"

gltriangle-riscv64: dynlib-riscv64
    cargo xtask build mesa riscv64 --stage
    @test -f target/mesa/riscv64/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa riscv64 --build` first' >&2; exit 1)
    cargo xtask build gltriangle riscv64
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (riscv64)"

gltriangle-aarch64: dynlib-aarch64
    cargo xtask build mesa aarch64 --stage
    @test -f target/mesa/aarch64/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa aarch64 --build` first' >&2; exit 1)
    cargo xtask build gltriangle aarch64
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (aarch64)"

# wlgl: build /bin/wlgl, the GL present client (§6.10 stage 3d-a). It is the tree's
# first *dynamically linked Rust* program, and it has to be one: `dlopen` needs a
# loader in the address space and a statically linked minix image has none
# (`crates/minix-libc/src/lib.rs`), so a program that loads Mesa must be dynamic.
# `cargo xtask build wlgl` is the link: the arch's C crt0 for `_start`, `minix-user.ld`,
# and Mesa's headers for the GL half (`tools/wlgl_render.c`).
wlgl-x86: userland-x86 dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'wlgl: no Mesa DSOs — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build wlgl x86
    @echo "wlgl: /bin/wlgl built and the Mesa DSOs staged (x86_64)"

# gui: build /bin/gui, the in-house renderer client (`gui.md` milestone M1). Like /bin/wlgl it
# is a dynamic image that loads Mesa, but the GL half is the `render` crate rather than a C
# object, and it draws a pass (clear, quad, glyph quads) instead of a triangle.
gui-x86: userland-x86 dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'gui: no Mesa DSOs — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build gui x86
    @echo "gui: /bin/gui built and the Mesa DSOs staged (x86_64)"

# gbmtest: build /bin/gbmtest, the GBM create/export probe (§6.10 stage 3d-b-3). It links
# `libgbm` and `libdrm` and includes the GBM/libdrm headers from the source checkouts, so it
# is built from `just build-mesa x86 --build` like the DSOs are.
gbmtest-x86: userland-x86 dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'gbmtest: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build gbmtest x86
    @echo "gbmtest: /bin/gbmtest built and the Mesa DSOs staged (x86_64)"

# wldmabufgl: build /bin/wldmabufgl, the 3d-b-4d GL present client (§6.10). Like /bin/wlgl it
# is a dynamic image that renders with Mesa, but it draws into the buffer it commits instead
# of reading the frame back: the render target is a GBM object on the render node, turned into
# a GL texture through an `EGLImage` over the object's descriptor, exported, and committed as
# a `zwp_linux_dmabuf_v1` buffer. The EGLImage route is the one Mesa's build here allows —
# `-Dplatforms=` leaves no `EGL_PLATFORM_GBM_KHR` — and `tools/egl_bo_probe.c` is the
# measurement that it works on the port.
wldmabufgl-x86: userland-x86 dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'wldmabufgl: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build wldmabufgl x86
    @echo "wldmabufgl: /bin/wldmabufgl built and the Mesa DSOs staged (x86_64)"

# wlcompose: build /bin/wlcompose, the 3d-b-5b GPU-composition helper (§6.10). Like the other Mesa
# gates it is a dynamic image; one binary is both the helper and its test peer, selected by argv
# (`serve`/`test`), which is what makes the cross-process measurement a real two-process one.
wlcompose-x86: userland-x86 dynlib-x86
    cargo xtask build mesa x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'wlcompose: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    cargo xtask build wlcompose x86
    @echo "wlcompose: /bin/wlcompose built and the Mesa DSOs staged (x86_64)"

# Measure whether two processes that map one object through the loader share its
# physical frames — Phase 5 of .notes/dynamic_linking.md. `cargo xtask probe dso-share` boots
# the dynamic image, runs `/bin/dynclib hold | /bin/dynclib hold` (two lives of the
# same dynamic image, `libc.so` mapped by the loader in each), then walks both
# processes' page tables from outside the guest and compares the frames behind the
# object's read-only pages. The two lives start together, which is the case that needs
# VM to join a fill already in flight; it is a probe rather than a smoke gate because it
# drives QEMU itself, so run it when VM's file fault path or the loader changes.
#
# `--arch` picks the page-table walk; Phase 5's gate asks for two arches, and these are
# the two it has. aarch64 needs its read-only bit (`AP[2]`) handled differently from the
# other two, so it is not a recipe yet.
probe-dso-share-x86: dynlink-x86
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/lib/libdyn3.so=target/dynlink/x86/libdyn3.so;/bin/dynhello=target/dynlink/x86/dynhello' just build-x86
    cargo xtask gate dso-share --arch x86

probe-dso-share-riscv64: dynlink-riscv64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/lib/libdyn3.so=target/dynlink/riscv64/libdyn3.so;/bin/dynhello=target/dynlink/riscv64/dynhello' just build-riscv64
    cargo xtask gate dso-share --arch riscv64

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
# and `cargo xtask gate` owns the guest: it bounds the run, kills a guest that
# hangs, and fails on a missing completion marker with the serial tail.

build-riscv64-test: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-test --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,integration-tests --release

build-aarch64-test: userland-aarch64 coreutils-aarch64 dynlib-aarch64
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64-test --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,integration-tests --release

build-riscv64-boot: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-boot --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,boot-test --release

build-aarch64-boot: userland-aarch64 coreutils-aarch64 dynlib-aarch64
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64-boot --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,boot-test --release

# Write the root filesystem blob to the per-arch
# target/images/<triple>/disk.img for the virtio-blk drive (kept separate
# per arch so one arch's mkfs never clobbers another's disk image).
mkfs target="x86":
    @just mkfs-{{target}}

mkfs-x86:
    cargo xtask image mkfs x86

mkfs-riscv64:
    cargo xtask image mkfs riscv64

mkfs-aarch64:
    cargo xtask image mkfs aarch64

# Rebuild the C smoke-test binaries (/bin/helloc, /bin/ctest) from tools/hello.c,
# tools/ctest.c and the arch's tools/crt0-<arch>.S (clang freestanding +
# minix-libc, linked with the fork rustc). For x86 it then re-embeds them in the
# initramfs and disk image with `cargo xtask image mkboot`; the other arches
# embed through their own `just build-<arch>` (`crates/kernel/build.rs`).
build-c-hello arch="x86":
    #!/bin/sh
    set -eu
    test -n "{{stage1-rustc}}" || { echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2; exit 1; }
    cargo xtask build c-hello {{arch}}
    # The re-embed is the x86 images'; the other arches embed through their own
    # `just build-<arch>` — crates/kernel/build.rs.
    if [ "{{arch}}" != x86 ]; then exit 0; fi
    cargo xtask image mkboot embed_initramfs,embed_minixfs
    cargo xtask image mkfs x86

# The C library as `libc.a`: the archive a C toolchain's own linker driver consumes,
# with no rustc in the link. It is the shape an in-guest clang/ld.lld needs, and it is
# also what LLVM's CMake links its try-compile and final executables against.
libc-static arch="x86":
    cargo xtask build libc-static {{arch}}

# A C program linked by clang and LLD alone, against `libc.a` and the arch's `crt0`.
# `cargo xtask gate c-direct --arch <arch>` boots it; the image base and `BASE_ADDRESS`/`LOADER_BASE`
# come from `Arch`, mirroring the fork target spec's `pre_link_args`.
c-direct arch="x86":
    @just libc-static {{arch}}
    cargo xtask build c-direct {{arch}}

# The host tablegen programs a cross build of clang needs. The fork's LLVM build
# tree carries `llvm-tblgen` only; `clang-tblgen` and `llvm-min-tblgen` come from
# this build. Artifact: `target/llvm/host/{Release/,}bin`.
llvm-host-tools:
    cargo xtask build llvm-host-tools

# Cross-build clang and LLD for a minix target and stage the guest's toolchain root
# (`.notes/toolchain_in_os.md` Phases 2 and 3). Needs `just llvm-host-tools` and
# the C++ runtime (`target/cxx/<machine>-toolchain/minix-runtime/libstdc++.a`);
# both are named in the error when missing. Artifacts: `target/llvm-<arch>/{build,
# toolchain.cmake}` and `target/toolchain/<arch>/root`, which is a *disk* image's
# root: 199 MiB, far past the embedded ramdisk's 16 MiB window.
llvm-minix arch="x86":
    cargo xtask build llvm-minix {{arch}}

# GNU bash for a minix target, from the upstream commit `tools/pins.ini` pins:
# fetched, configured against `tools/c-include` and linked against minix-libc by
# `cargo xtask cc minix`, the `cc` a C project's own build needs. That host must be
# POSIX (the stage1, the rlib and clang all have to be the same host's), so on
# Windows `cargo xtask build bash` re-enters WSL by itself; `MINIX_WSL_DISTRO`
# picks the distribution. Artifact: `target/bash/<arch>/bash`; `cargo xtask gate
# bash --arch <arch>` boots it. `.notes/c_build.md` has the why of each flag and each trap this
# build costs.
#
# Run this *after* the toolchain: `bootstrap` and `fetch-stage1` start with
# `cargo clean`, which takes `target/bash-src` and the build tree with it.
build-bash arch="x86" *args:
    cargo xtask build bash {{arch}} {{args}}

# Install the prebuilt bash for the pinned commit (`tools/pins.ini`) instead of
# building it: the release `.github/workflows/bash-release.yml` publishes, tagged
# by that commit. `just build-bash` stays authoritative (a C header or libc change
# needs it); this is the cache CI and dev machines use.
fetch-bash arch="x86" *args:
    cargo xtask fetch bash {{arch}} {{args}}

# Create or refresh the `sdgoij/bash` GitHub mirror and push it, so the build never
# clones savannah (it is slow and times out) — `cargo xtask mirror bash` takes the
# objects from upstream, verifies the pinned commit (`tools/pins.ini`) is present,
# and pushes. Needs an empty `sdgoij/bash` in the org and a key that can push to it.
# `just mirror-bash --check` only verifies the pin, without pushing.
mirror-bash *args:
    cargo xtask mirror bash {{args}}

# Fetch and configure Mesa + libdrm for a target (§6.10 stage 3c-0): pinned sources
# under `target/`, a meson cross file naming `cargo xtask cc dso`, and `meson
# setup` for the softpipe branch. Meson and ninja must be on the host's PATH (a
# venv install is enough). `--build` also runs ninja and stages the DSOs;
# `--stage` only copies them into `target/mesa/<arch>/lib`.
build-mesa arch="x86" *args:
    cargo xtask build mesa {{arch}} {{args}}

# Install the prebuilt Mesa + libdrm DSOs for the pinned version
# (`tools/pins.ini`) instead of building them: the release
# `.github/workflows/mesa-release.yml` publishes, tagged by the pinned Mesa and
# libdrm commits. `just build-mesa` stays authoritative (it is what a pin bump or
# a compiler change needs); this is the cache CI and dev machines use.
fetch-mesa arch="x86" *args:
    cargo xtask fetch mesa {{arch}} {{args}}

# The dynamic-linking artifacts an *image* carries: the loader (`/libexec/ld.so`,
# `crates/ldso` linked with its own script, `tools/minix-ldso.ld`), the shared C library
# (`/lib/libc.so`) and a C program linked against it (`/bin/dynclib`). All three are in
# `crates/boot-image/src/manifest.rs`'s `BOOT_BINS`, so every recipe that assembles an
# image depends on this one, and an image has dynamic linking whether or not its gate was
# ever run. The loader is what a `PT_INTERP` binary needs to be exec'd at all; nothing in
# the boot path carries one, so an image with these three boots exactly as before
# (`.notes/dynamic_linking.md` D2).
#
# The loader lands in the release dir from cargo, and `cargo xtask build dynlibc` puts the
# other two there, the way `cargo xtask build c-hello` does for helloc/ctest.
dynlib-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found - run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target x86_64-pc-minix --release
    cargo xtask build dynlibc x86

# What the loader's *own* gate needs on top of `dynlib-x86`: `cargo xtask build dynlink`
# builds `libdyn*.so` and `dynhello` (resolution, the region budget), `libdlopen.so`,
# `libtls1.so` and `dynopen` (the `dlopen` steps), whose strings live only in the objects
# and which no image ships. The `dynlink` gate injects them into a standard image
# (`DYNLINK_BINS`) and boots it.
dynlink-x86: dynlib-x86
    cargo xtask build dynlink x86

# Build a C program of your own against the shared C library. `just cdyn tools/myprog.c`
# writes `target/dync/<arch>/<stem>`, and the program runs on a booted system with nothing
# else added: every image already carries `/lib/libc.so` and the loader the program asks
# for. `cargo xtask build cdyn` then prints the two commands that boot it — the second sets
# `MINIXFS_EXTRA`, and on Windows it is prefixed with `MSYS2_ENV_CONV_EXCL` so the `/bin/...`
# value survives MSYS's path conversion (a value that does not is refused by
# `crates/kernel/build.rs`).
#
# `cargo xtask build cdyn` is the same link `cargo xtask build dynlibc` gives the shipped
# `/bin/dynclib`, so a program of your own gets the flags that program is tested with, and
# `cargo xtask gate cdyn --arch x86` is the gate on this recipe.
cdyn src arch="x86":
    cargo xtask build cdyn {{src}} --target {{arch}}

# Build the C++ runtime (libc++ + libc++abi) for one minix target and merge them
# into target/cxx/<arch>/minix-runtime/libstdc++.a — the archive Mesa's shared
# objects link (`cargo xtask cc dso`). x86 was the first arch; the others build
# the same way and only differ in the red-zone/march flags (`crates/xtask/src/arch.rs`).
#
# Freestanding CMake cross builds (host clang targeting <machine>-unknown-none)
# against the Minix C headers (tools/c-include), driven by
# `cargo xtask build libcxx`, which owns the quirks this recipe used to spell out:
#   - both libs need a distinct *_SHARED_OUTPUT_NAME: the never-built shared
#     target collides with the static archive name on the Generic platform
#     (ninja "multiple rules generate lib/libc++.a")
#   - libc++ builds with threads on now that the port's pthread has rwlock,
#     barrier and condattr; libc++abi keeps RTTI (private_typeinfo.cpp uses
#     dynamic_cast), so it uses the plain toolchain, not the -fno-rtti LLVM one
#   - include order must be libc++ -> c-include -> libcxx-build config, or
#     libc++'s <string.h> guard skips the C headers and ::size_t is missing
#   - the IWYU mapping step needs Python3_EXECUTABLE; the standalone libcxx
#     cmake never sets it, and on Windows the only python (the embeddable
#     CPython beside LLVM, whose python311._pth pins sys.path) cannot import
#     libcxx's local modules, so tools/libcxx-iwyu.cmd stubs the step
libcxx arch="x86":
    cargo xtask build libcxx {{arch}}

# ---------- gate preparation ----------
#
# `cargo xtask gate <name> --arch <arch>` runs one of these before it boots the
# guest, so a gate is one command. They are build recipes: none of them boots
# anything. The default is `build <arch>` (an embedded image) or
# `system-image <arch>` (a boot image plus the disk); the gates whose artifact
# needs an injected file or a Mesa disk name their own recipe here.

prepare-image arch="x86":
    @just prepare-image-{{arch}}

prepare-image-x86:
    @just build-x86

prepare-image-riscv64:
    MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" just build-riscv64

prepare-image-aarch64:
    @just build-aarch64

prepare-bash arch="x86":
    @just prepare-bash-{{arch}}

prepare-bash-x86: (build-bash "x86")
    MINIXFS_EXTRA='/bin/bash=target/bash/x86/bash' just build-x86

prepare-bash-riscv64: (build-bash "riscv64")
    MINIXFS_EXTRA='/bin/bash=target/bash/riscv64/bash' MINIXFS_BLOCKS=8192 just build-riscv64

prepare-bash-aarch64: (build-bash "aarch64")
    MINIXFS_EXTRA='/bin/bash=target/bash/aarch64/bash' just build-aarch64

prepare-dynlink arch="x86":
    @just prepare-dynlink-{{arch}}

prepare-dynlink-x86: dynlink-x86
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/lib/libdyn3.so=target/dynlink/x86/libdyn3.so;/bin/dynhello=target/dynlink/x86/dynhello;/lib/libdlopen.so=target/dynlink/x86/libdlopen.so;/lib/libtls1.so=target/dynlink/x86/libtls1.so;/bin/dynopen=target/dynlink/x86/dynopen' just build-x86
    @if grep -q 'dynlink' target/dynlink/x86/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/x86_64-pc-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/x86/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi

prepare-dynlink-riscv64: dynlink-riscv64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/lib/libdyn3.so=target/dynlink/riscv64/libdyn3.so;/bin/dynhello=target/dynlink/riscv64/dynhello;/lib/libdlopen.so=target/dynlink/riscv64/libdlopen.so;/lib/libtls1.so=target/dynlink/riscv64/libtls1.so;/bin/dynopen=target/dynlink/riscv64/dynopen' just build-riscv64
    @if grep -q 'dynlink' target/dynlink/riscv64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/riscv64gc-unknown-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/riscv64/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi

prepare-dynlink-aarch64: dynlink-aarch64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/aarch64/libdyn.so;/lib/libdyn2.so=target/dynlink/aarch64/libdyn2.so;/lib/libdyn3.so=target/dynlink/aarch64/libdyn3.so;/bin/dynhello=target/dynlink/aarch64/dynhello;/lib/libdlopen.so=target/dynlink/aarch64/libdlopen.so;/lib/libtls1.so=target/dynlink/aarch64/libtls1.so;/bin/dynopen=target/dynlink/aarch64/dynopen' just build-aarch64
    @if grep -q 'dynlink' target/dynlink/aarch64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/aarch64-unknown-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/aarch64/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi

prepare-dso-share arch="x86":
    @just prepare-dso-share-{{arch}}

prepare-dso-share-x86: dynlink-x86
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/lib/libdyn3.so=target/dynlink/x86/libdyn3.so;/bin/dynhello=target/dynlink/x86/dynhello' just build-x86

prepare-dso-share-riscv64: dynlink-riscv64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/lib/libdyn3.so=target/dynlink/riscv64/libdyn3.so;/bin/dynhello=target/dynlink/riscv64/dynhello' just build-riscv64

prepare-c-direct arch="x86":
    @just prepare-c-direct-{{arch}}

prepare-c-direct-x86: (c-direct "x86")
    MINIXFS_EXTRA='/bin/cdirect=target/x86_64-pc-minix/release/cdirect' just build-x86

prepare-c-direct-riscv64: (c-direct "riscv64")
    MINIXFS_EXTRA='/bin/cdirect=target/riscv64gc-unknown-minix/release/cdirect' just build-riscv64

prepare-c-direct-aarch64: (c-direct "aarch64")
    MINIXFS_EXTRA='/bin/cdirect=target/aarch64-unknown-minix/release/cdirect' just build-aarch64

prepare-cdyn-x86: dynlib-x86
    just cdyn tools/cdyn-demo.c x86
    @if grep -q 'No such file or directory' target/dync/x86/cdyn-demo; then echo "!! the program contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    MINIXFS_EXTRA='/bin/cdyn-demo=target/dync/x86/cdyn-demo' just build-x86

prepare-wlfull arch="x86":
    @just prepare-wlfull-{{arch}}

prepare-wlfull-x86:
    @just build-x86

prepare-wlfull-riscv64:
    @just system-image-riscv64

prepare-wlfull-aarch64:
    @just system-image-aarch64

prepare-gltriangle arch="x86":
    @just prepare-gltriangle-{{arch}}

prepare-gltriangle-x86: gltriangle-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-gltriangle-riscv64: gltriangle-riscv64 boot-image-riscv64
    M=target/mesa/riscv64; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- riscv64
    @just mkfs-riscv64

prepare-gltriangle-aarch64: gltriangle-aarch64 boot-image-aarch64
    M=target/mesa/aarch64; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- aarch64
    @just mkfs-aarch64

prepare-gltriangle-virgl-x86: gltriangle-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-wlgl-x86: wlgl-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/bin/wlgl=$M/bin/wlgl"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-gui-x86: gui-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gui=$M/bin/gui"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-gbmtest-x86: gbmtest-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/gbmtest=$M/bin/gbmtest"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-wldmabufgl-x86: wldmabufgl-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wldmabufgl=$M/bin/wldmabufgl"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-wlcompose-x86: wlcompose-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wlcompose=$M/bin/wlcompose"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-wlscene-x86: wlcompose-x86 boot-image-x86
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wlcompose=$M/bin/wlcompose"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

prepare-build-test arch="x86":
    @just build-{{arch}}-test

prepare-build-boot arch="x86":
    @just build-{{arch}}-boot

prepare-mkfs arch="x86":
    @just mkfs-{{arch}}

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
check-linux:
    @podman machine start >/dev/null 2>&1 || true
    MSYS_NO_PATHCONV=1 podman run --rm --network=host -e RUSTUP_TOOLCHAIN={{rust-channel}}-x86_64-unknown-linux-gnu -e CARGO_TARGET_DIR=/tmp/just-target -v "{{ROOT}}:/w:ro" -w /w rust:{{rust-channel}} bash -c 'export PATH=/usr/local/cargo/bin:/usr/bin:/bin; cargo test --workspace --no-fail-fast --locked 2>&1 | tee /tmp/t.log; t=${PIPESTATUS[0]}; grep -h "^test result:" /tmp/t.log | awk "{p+=\$4;i+=\$8} END {printf \"linux host tests: passed=%d ignored=%d summaries=%d\\n\",p,i,NR}"; if [ $t -ne 0 ]; then echo "--- failures ---"; grep -nE "test result: FAILED|^error|signal:|panicked" /tmp/t.log | head -20; fi; rustup component add clippy >/dev/null 2>&1; cargo clippy --all-targets -- -D warnings; c=$?; echo "linux host tests: cargo-test rc=$t clippy rc=$c"; test $t -eq 0 -a $c -eq 0'

# Host clippy, riscv64 compilation check (fork stage1 compiler), and the C
# header contract: the headers in tools/c-include are what the C and C++
# consumers compile against (bash, /bin/helloc and /bin/ctest, the libc++
# build), and nothing else notices when an export and its declaration drift
# apart. The checker parses the Rust sources, so it needs no cbindgen.
#
# Clippy runs with `--all-targets` so the lint set matches CI's `host-tests`
# job, which lints tests, examples and bins as well as the libraries.
#
# The physmap rule is the same kind of gate (`.notes/physmap.md` P6): a physical address is a
# number, not a pointer, and `cargo xtask check physmap` rejects one cast to a pointer unless
# the line converts it (`phys_to_virt`/`frame_ptr`/`table_ptr`) or says `physmap-ok: <reason>`.
#
# The Justfile check pins what a clean tree needs and a warm `target/` hides: a recipe that
# builds `kernel-boot` must depend on the recipes that produce every `BOOT_BINS` entry.
#
# `just check` is the commit gate: clippy, the static checks, and a riscv64 kernel compile.
check:
    cargo clippy --all-targets -- -D warnings
    @just check-static
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" cargo check -p kernel-boot --bin kernel-boot-riscv64 --features riscv64 --target riscv64gc-unknown-minix --release

# The C header contract, the physmap rule and the Justfile's image prerequisites, all
# three in `crates/xtask` (`cargo xtask check static`). CI's `host-tests` job runs
# this on its own rather than the whole of `check`, which would repeat that job's
# clippy pass and want a stage1 it does not install.
#
# `check-static` is the toolchain-free half of the commit gate.
check-static:
    cargo xtask check static

# Which repositories have commits their remote does not: the superproject and every
# fork, each compared with its own upstream. Nested vendored submodules are skipped,
# since those are never something this project pushes. `--fetch` refreshes the
# remote-tracking refs first, `--commits` lists the subjects. Exits 1 when something
# is waiting, which is why the recipe does not turn that into an error line.
push-status *args:
    -cargo xtask push-status {{args}}

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
    rm -f target/initramfs.cpio target/minixfs.img target/trampoline.elf target/trampoline_.o target/kernel.bin
