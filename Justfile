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
# The recipes orchestrate plain `cargo` invocations. The kernel-embedded image
# (initramfs CPIO + MinixFS) is assembled by `crates/kernel/build.rs`; the *disk*
# system image is assembled by `boot-image`'s `mkminixfs`, which reads
# `MINIXFS_EXTRA` / `DYNLINK_BINS` / `MINIXFS_BLOCKS`. x86 post-link work
# (trampoline + kernel.bin) lives in `tools/mkboot.rs`.

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

# Type-check smithay's `wayland_frontend` tree for the minix targets.
smithay target="all":
    python tools/build-smithay.py {{target}}

# The same, with `cargo build` (rlibs) instead of `cargo check`.
smithay-build target="all":
    python tools/build-smithay.py {{target}} --build

# build-x86* embeds /bin/coreutils, so the multicall must be built and copied
# into the shared target dir first.
build-x86: userland-x86 coreutils-x86 dynlib-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs

build-x86-test: userland-x86 coreutils-x86 dynlib-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,integration-tests kernel-test

build-x86-boot: userland-x86 coreutils-x86 dynlib-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,boot-test kernel-boot

# The boot image: kernel + initramfs only. This is what a disk-root boot needs, so
# the kernel carries no root filesystem and is not capped by the 16 MiB ramdisk
# window (`RAMDISK_IMAGE_SIZE`; an embedded root past it lands on the user mmap
# region at 4 GiB). Output: target/boot-kernel.bin + target/boot-kernel-trampoline.elf,
# mirrored to target/images/<triple>/minix-x86-boot.elf for the release.
boot-image-x86: userland-x86 coreutils-x86 dynlib-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs boot-kernel
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
system-image-x86: boot-image-x86
    extras=""; if [ -f target/bash/x86/bash ]; then extras=/bin/bash=target/bash/x86/bash; else echo "system-image: no /bin/bash (run 'just build-bash x86' to include it)"; fi; MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86

# riscv64 / aarch64, the same split without mkboot: the kernel ELF *is* the artifact
# (`-kernel`). The boot image is copied to a distinct name so it does not collide with
# the self-contained `release/kernel-boot-<arch>` that the tests and `image-<arch>` boot.
boot-image-riscv64: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,riscv64 --release
    cp target/riscv64gc-unknown-minix/release/kernel-boot-riscv64 target/boot-kernel-riscv64.elf
    mkdir -p target/images/riscv64gc-unknown-minix
    cp target/boot-kernel-riscv64.elf target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf

system-image-riscv64: boot-image-riscv64
    extras=""; if [ -f target/bash/riscv64/bash ]; then extras=/bin/bash=target/bash/riscv64/bash; else echo "system-image: no /bin/bash (run 'just build-bash riscv64' to include it)"; fi; MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- riscv64
    @just mkfs-riscv64

boot-image-aarch64: userland-aarch64 dynlib-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,aarch64 --release
    cp target/aarch64-unknown-minix/release/kernel-boot-aarch64 target/boot-kernel-aarch64.elf
    mkdir -p target/images/aarch64-unknown-minix
    cp target/boot-kernel-aarch64.elf target/images/aarch64-unknown-minix/minix-aarch64-boot.elf

system-image-aarch64: boot-image-aarch64
    extras=""; if [ -f target/bash/aarch64/bash ]; then extras=/bin/bash=target/bash/aarch64/bash; else echo "system-image: no /bin/bash (run 'just build-bash aarch64' to include it)"; fi; MINIXFS_EXTRA="${extras}${MINIXFS_EXTRA:+;$MINIXFS_EXTRA}" MINIXFS_BLOCKS="${MINIXFS_BLOCKS:-8192}" cargo run -q -p boot-image --bin mkminixfs --release -- aarch64
    @just mkfs-aarch64

# Boot the split pair and drive the userspace smoke scenario into its shell, so a
# release only ships a system image that was actually booted (the self-contained
# `image-<arch>` is booted the same way). The dependency builds both images first.
test-system-image-x86 boot-timeout="40": system-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    sh tools/smoke/feed.sh target/test-system-image-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @just _assert-qemu-log target/test-system-image-x86.log "wserver: ready"

test-system-image-riscv64 boot-timeout="60": system-image-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    sh tools/smoke/feed.sh target/test-system-image-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf
    @just _assert-qemu-log target/test-system-image-riscv64.log "wserver: ready"

test-system-image-aarch64 boot-timeout="60": system-image-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    sh tools/smoke/feed.sh target/test-system-image-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64-boot.elf
    @just _assert-qemu-log target/test-system-image-aarch64.log "wserver: ready"

# The `/dev/fb` `mmap` path: a userland `mmap` of the framebuffer must alias the device's
# own memory, which every present path (`wserver`, `/sbin/wlserver`, and the clients that
# read the frame back) depends on. It broke silently once: after `VM_MAP_PHYS` began
# returning a *window* rather than the physical address (`.notes/physmap.md` D5), the fb
# server reported that window as the physical base, so every client's `mmap` landed where
# the device is not and read a black frame. `/bin/fbmmap` does the whole check itself —
# paint, then read back the block and the boot pattern — so this needs no screenshot and
# no GL host, and it runs on every arch.
test-fbmmap arch="x86" boot-timeout="40":
    @just test-fbmmap-{{arch}} {{boot-timeout}}

test-fbmmap-x86 boot-timeout="40": system-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/fbmmap.tsv sh tools/smoke/feed.sh target/test-fbmmap-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "fbmmap: a userland mmap of /dev/fb aliases the live framebuffer (x86_64)"

test-fbmmap-riscv64 boot-timeout="60": system-image-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/fbmmap.tsv sh tools/smoke/feed.sh target/test-fbmmap-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf
    @echo "fbmmap: a userland mmap of /dev/fb aliases the live framebuffer (riscv64)"

test-fbmmap-aarch64 boot-timeout="60": system-image-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/fbmmap.tsv sh tools/smoke/feed.sh target/test-fbmmap-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64-boot.elf
    @echo "fbmmap: a userland mmap of /dev/fb aliases the live framebuffer (aarch64)"

# Interactive bash: readline polls the console instead of blocking on a line read,
# so this drives a *typed* line at `bash-5.3#` — the part the `bash -c` scenario in
# `test-bash-<arch>` cannot see. It boots the split pair (the shipping artifact),
# which carries bash when its `target/bash/<arch>/bash` exists; `build-bash` first
# so the image the gate builds has it.
test-interactive-bash arch="x86" boot-timeout="60": (build-bash arch)
    @just test-interactive-bash-{{arch}} {{boot-timeout}}

test-interactive-bash-x86 boot-timeout="40": system-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    python tools/smoke/interactive_bash.py target/test-interactive-bash-x86.log {{boot-timeout}} -- qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0

test-interactive-bash-riscv64 boot-timeout="60": system-image-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    python tools/smoke/interactive_bash.py target/test-interactive-bash-riscv64.log {{boot-timeout}} -- qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf

test-interactive-bash-aarch64 boot-timeout="60": system-image-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    python tools/smoke/interactive_bash.py target/test-interactive-bash-aarch64.log {{boot-timeout}} -- qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64-boot.elf

build-riscv64: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64 --release

build-aarch64: userland-aarch64 dynlib-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64 --release

# ---------- run ----------

run target="x86" memory="256M":
    @just run-{{target}} {{memory}}

run-x86 memory: boot-image-x86 system-image-x86
    qemu-system-x86_64 -nographic -m {{memory}} -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/boot-kernel-trampoline.elf -device loader,file=target/boot-kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0

run-riscv64 memory: boot-image-riscv64 system-image-riscv64
    qemu-system-riscv64 -machine virt -m {{memory}} -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/boot-kernel-riscv64.elf

run-aarch64 memory: boot-image-aarch64 system-image-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m {{memory}} -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/boot-kernel-aarch64.elf

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
    @just _assert-qemu-log target/image-x86.log "fb: gpu3d none"
    @echo "done: target/images/x86_64-pc-minix/minix-x86.elf — qemu-system-x86_64 -nographic -m 256M -kernel <it>"


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
# feed.sh drives.
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
# The *degrade* half is graded on every plain x86 boot: `image-x86` asserts
# `fb: gpu3d none`, and `test-gpu3d-nogl-x86` covers the other degrade case, a device
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

test-gpu3d-x86 boot-timeout="30": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    /usr/bin/timeout -s 9 {{boot-timeout}} qemu-system-x86_64 -display egl-headless -serial stdio -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-gl-pci 2>&1 | tee target/test-gpu3d-x86.log
    @just _assert-qemu-log target/test-gpu3d-x86.log "wserver: ready"
    @just _assert-qemu-log target/test-gpu3d-x86.log "wlserver: ready"
    @just _assert-qemu-log target/test-gpu3d-x86.log "fb: gpu3d virgl capset"
    @just _assert-qemu-log target/test-gpu3d-x86.log "xfer 16384/16384"
    @just _assert-qemu-log target/test-gpu3d-x86.log "submit 16384/16384"
    @echo "gpu3d: the render node negotiated virgl, answered the capset query, and round-tripped a texture (x86_64)"

# The other half of 3a's degrade path, and the one `image-x86` cannot cover: a
# virtio-gpu device the host offers but did *not* enable GL on. Such a device answers
# no VIRTIO_GPU_F_VIRGL, and the probe has to say so and stop there - the capset and
# context commands it would otherwise send are exactly the ones a feature-less device
# answers with errors (or, worse, does not answer at all). A plain `virtio-gpu-pci` is
# that device on every x86 host, GL-capable or not, and being 2D-only it needs no GL
# display backend: `-nographic` is enough, so this one boot carries both halves of 3b-2:
# the fb server reports the device as no-3D, and then a client opens the render node and
# is told so itself. The step is the client rather than the boot smoke because the client
# is the stronger proof that the guest got here — it execs a binary from the image, opens
# a device, and takes a path through VFS and a driver — and because the smoke proper is
# `image-x86`'s. `tools/smoke/drminfo.tsv` has the rest of the reason this gate uses a
# device without GL.
test-gpu3d-nogl-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/drminfo.tsv sh tools/smoke/feed.sh target/test-gpu3d-nogl-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-pci
    @just _assert-qemu-log target/test-gpu3d-nogl-x86.log "fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"
    @echo "gpu3d: a device the host gave no GL to was reported as no 3D by the fb server and by a client opening its render node (x86_64)"

# The node's *memory objects*, which is 3b-3 and a different claim from 3b-2's: a client
# creates one, maps it, and proves the mapping is that object's memory. Same device as the
# gate above and for the same reasons (it types, so it needs the 2D-only device; and
# `RESOURCE_CREATE` on a device without GL is the `RESOURCE_CREATE_2D` +
# `ATTACH_BACKING` path, which a later client meets whenever the host has no GL).
test-drmmap-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/drmmap.tsv sh tools/smoke/feed.sh target/test-drmmap-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-pci
    @just _assert-qemu-log target/test-drmmap-x86.log "fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"
    @echo "drmmap: a client created a render node memory object, mapped it twice, found one set of frames, saw a second object be other memory, and closed it (x86_64)"

# The render node's PRIME handoff (`tools/smoke/drmprime.tsv`): `/bin/drmprime` exports a
# memory object to a dma-buf descriptor, checks the descriptor's own kind and device with
# `fstat`, imports it back to the object's handle, and reads the bytes through it. The device
# is the same plain `virtio-gpu-pci` the other node gates use, for their reason: sharing is a
# property of the node's object table, not of GL, so the degraded device exercises the same
# code without a GL host to drop serial bytes.
test-drmprime-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/drmprime.tsv sh tools/smoke/feed.sh target/test-drmprime-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-pci
    @just _assert-qemu-log target/test-drmprime-x86.log "fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"
    @echo "drmprime: a client exported a render node object to a dma-buf descriptor and imported it back, and the bytes survived (x86_64)"

# `readlink(2)`, which nothing proved until now: the three pieces that move a filesystem
# server's bytes into a user process (`tools/smoke/readlink.tsv` has the whole reason) are all
# stubs of some kind before this, so the gate is the first exercise of the path. No device is
# needed beyond the ones a plain boot has, and the step's own expectation is the assertion.
test-readlink-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/readlink.tsv sh tools/smoke/feed.sh target/test-readlink-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "readlink: a client read the target of the symlink the image ships, and the refusals a read has to give (x86_64)"

# `symlink(2)`, the write half of the readlink gate: a link MFS creates at run time, read
# back out through the same path (`tools/smoke/symlink.tsv` has the whole reason). Nothing
# had ever created one, so this is the first exercise of it, and the step's own expectation
# is the assertion. No device is needed beyond the ones a plain boot has.
test-symlink-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/symlink.tsv sh tools/smoke/feed.sh target/test-symlink-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "symlink: a client created a link in /tmp, read its target back through the grant, and got the three refusals (x86_64)"

# `link(2)` and `rename(2)`, the acceptance test for KNOWN_ISSUES.md item 37 - green since that
# item's faults were fixed (2026-10-02). It was *red* on purpose until then, and written to the
# behaviour the calls should have rather than the behaviour they had, so no step here was edited
# when the fix landed: the same file that measured the fault is what now passes.
#
# A step's fields are separate claims - each call's own errno, the state it left, and a
# neighbour file's bytes - so a call that ran against the wrong name cannot read as one that
# worked. x86 only: the fault was in VFS/MFS, which the other arches share.
test-link-x86 boot-timeout="20": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/link.tsv sh tools/smoke/feed.sh target/test-link-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "link: a hard link and a rename carried their names (x86_64)"

# The render node's client on the GL device, which is the one gate that types into a guest with a
# GL display backend: host GL drops serial bytes mid-line, so this recipe sets `FEED_PACE` and
# `feed.sh` writes the step a byte at a time and refuses to believe it until the guest has echoed
# the whole line (`tools/smoke/drmgl.tsv` has the measurements and the rest of the reason).
#
# `blob=on` because the blob feature is only available on this host's GL device, and the two
# assertions below are the two ends of it: the fb server's boot line says the device offered it,
# and the client's line says the node reports it.
test-drmgl-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/drmgl.tsv sh tools/smoke/feed.sh target/test-drmgl-x86.log {{boot-timeout}} qemu-system-x86_64 -display egl-headless -serial stdio -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-gl-pci,blob=on
    @just _assert-qemu-log target/test-drmgl-x86.log "wserver: ready"
    @just _assert-qemu-log target/test-drmgl-x86.log "wlserver: ready"
    @just _assert-qemu-log target/test-drmgl-x86.log "capsets 2 blob 1"
    @echo "drmgl: a client was driven into a guest with a GL display, and the node answered its capset query with the device's blobs on (x86_64)"

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
# for three shared objects, one of which names another three times, so the strings that
# live only in those objects are what prove the loader chose a base for each of them,
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
# The injection adds the loader's *own* test objects to a standard image: `libdyn*.so`
# and `/bin/dynhello` are deliberately not `BOOT_BINS`, so no image carries them. What
# they need in order to run — the loader, `/lib/libc.so` and `/bin/dynclib` — an image
# does carry, and this gate is where the shipped `/bin/dynclib` is exercised, so a
# regression in it is caught here rather than only in a gate that had built its own
# copy.
#
# The last five steps are `dlopen` (Phase 6). `tools/dynopen.c` asks the loader for
# `libdlopen.so` at run time — the object is in nothing's `DT_NEEDED`, which is the case a
# driver lookup is — and reaches it through the handle, and through `RTLD_DEFAULT` after an
# `RTLD_GLOBAL` load, and again after a `dlclose`. The two failing steps assert the loader's
# own message rather than just "it failed": a path that is not there, and `libtls1.so`,
# whose thread-local the loader cannot place, because the module it lays out is fixed at
# startup for every thread.
test-dynlink-x86 boot-timeout="40": dynlink-x86
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/x86/libdyn.so;/lib/libdyn2.so=target/dynlink/x86/libdyn2.so;/lib/libdyn3.so=target/dynlink/x86/libdyn3.so;/bin/dynhello=target/dynlink/x86/dynhello;/lib/libdlopen.so=target/dynlink/x86/libdlopen.so;/lib/libtls1.so=target/dynlink/x86/libtls1.so;/bin/dynopen=target/dynlink/x86/dynopen' just build-x86
    @if grep -q 'dynlink' target/dynlink/x86/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/x86_64-pc-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/x86/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (x86_64)"

# The same artifacts for riscv64. `tools/build-dynlink.py` and
# `tools/build-dynlibc.py` take the target; only the loader's `_start` and its TLS
# placement differ between them (`crates/ldso`).
dynlib-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target riscv64gc-unknown-minix --release
    python tools/build-dynlibc.py riscv64

dynlink-riscv64: dynlib-riscv64
    python tools/build-dynlink.py riscv64

# The dynamic-linking gate for riscv64: the same `tools/smoke/dyn.tsv` scenario as
# x86_64, which is deliberately arch-neutral — it asserts on symbols only the shared
# objects contain, on an errno read through the loader's TLS, and on the program's
# own constructor, none of which depends on the instruction set.
test-dynlink-riscv64 boot-timeout="60": dynlink-riscv64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/lib/libdyn3.so=target/dynlink/riscv64/libdyn3.so;/bin/dynhello=target/dynlink/riscv64/dynhello;/lib/libdlopen.so=target/dynlink/riscv64/libdlopen.so;/lib/libtls1.so=target/dynlink/riscv64/libtls1.so;/bin/dynopen=target/dynlink/riscv64/dynopen' just build-riscv64
    @if grep -q 'dynlink' target/dynlink/riscv64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/riscv64gc-unknown-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/riscv64/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (riscv64)"

dynlib-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target aarch64-unknown-minix --release
    python tools/build-dynlibc.py aarch64

dynlink-aarch64: dynlib-aarch64
    python tools/build-dynlink.py aarch64

test-dynlink-aarch64 boot-timeout="60": dynlink-aarch64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/aarch64/libdyn.so;/lib/libdyn2.so=target/dynlink/aarch64/libdyn2.so;/lib/libdyn3.so=target/dynlink/aarch64/libdyn3.so;/bin/dynhello=target/dynlink/aarch64/dynhello;/lib/libdlopen.so=target/dynlink/aarch64/libdlopen.so;/lib/libtls1.so=target/dynlink/aarch64/libtls1.so;/bin/dynopen=target/dynlink/aarch64/dynopen' just build-aarch64
    @if grep -q 'dynlink' target/dynlink/aarch64/dynhello; then echo "!! /bin/dynhello contains a 'dynlink' string - the message cannot have come from a shared object" >&2; exit 1; fi
    @if grep -q 'No such file or directory' target/aarch64-unknown-minix/release/dynclib; then echo "!! /bin/dynclib contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    @if grep -q 'dynopen-text' target/dynlink/aarch64/dynopen; then echo "!! /bin/dynopen contains the object's text - the line cannot have come from libdlopen.so" >&2; exit 1; fi
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/dyn.tsv sh tools/smoke/feed.sh target/test-dynlink-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so (aarch64)"

# The gate on `just cdyn` — the recipe a user is told to use for a program of their own.
# Every other recipe here builds its programs with `tools/build-dynlink.py` or
# `tools/build-dynlibc.py`; this one goes through the recipe's own interface and boots what
# it produced, which is what makes the two commands `tools/cdyn.py` prints for a user
# something that has been run at least once. The link underneath is that script's, the same
# one the shipped `/bin/dynclib` gets.
#
# The injection is `MINIXFS_EXTRA` set inside the recipe, so the Justfile's
# `MSYS2_ENV_CONV_EXCL` covers it. A user setting the variable in their own shell has to
# cover it themselves, which is why the hint `tools/cdyn.py` prints names that variable on
# Windows.
test-cdyn-x86 boot-timeout="40": dynlib-x86
    just cdyn tools/cdyn-demo.c x86
    @if grep -q 'No such file or directory' target/dync/x86/cdyn-demo; then echo "!! the program contains the error message - it cannot have come from libc.so" >&2; exit 1; fi
    MINIXFS_EXTRA='/bin/cdyn-demo=target/dync/x86/cdyn-demo' just build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/cdyn.tsv sh tools/smoke/feed.sh target/test-cdyn-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "cdyn: a program built by the recipe ran against the shipped libc.so (x86_64)"

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
    python tools/build-mesa.py x86 --stage
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa x86 --build` first' >&2; exit 1)
    python tools/build-gltest.py x86
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (x86_64)"

gltriangle-riscv64: dynlib-riscv64
    python tools/build-mesa.py riscv64 --stage
    @test -f target/mesa/riscv64/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa riscv64 --build` first' >&2; exit 1)
    python tools/build-gltest.py riscv64
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (riscv64)"

gltriangle-aarch64: dynlib-aarch64
    python tools/build-mesa.py aarch64 --stage
    @test -f target/mesa/aarch64/lib/libEGL.so.1 || (echo 'gltriangle: no Mesa DSOs — run `just build-mesa aarch64 --build` first' >&2; exit 1)
    python tools/build-gltest.py aarch64
    @echo "gltriangle: /bin/gltriangle built and the Mesa DSOs staged (aarch64)"

# The §6.10 stage 3c-2 gate: boot the split pair with /bin/gltriangle and the seven
# Mesa DSOs in the *system* image and drive `tools/smoke/gltriangle.tsv`, whose one
# step wants the client's `gltriangle: pass` line. This is the end-to-end proof of the
# C GL stack — surfaceless EGL, the gallium `softpipe` driver, GLSL and a checked
# readback — and of the loader's `DTPREL` (a general-dynamic thread-local access).
# The DSOs are ~36 MiB, so the system image carries them with `MINIXFS_BLOCKS` raised.
# Needs `just build-mesa <arch> --build` and `just libcxx <arch>` first.
test-gltriangle arch="x86" boot-timeout="180":
    @just test-gltriangle-{{arch}} {{boot-timeout}}

test-gltriangle-x86 boot-timeout="180": gltriangle-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    FEED_SCENARIO=tools/smoke/gltriangle.tsv sh tools/smoke/feed.sh target/test-gltriangle-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "gltriangle: the GLES2 triangle rendered through surfaceless EGL on softpipe (x86_64)"

# The 3c-3 gate: the *same* client as 3c-2, differing only in the device Mesa is
# handed. The guest gets a GL device (`-device virtio-gpu-gl-pci,blob=on`), so its
# render node opens and surfaceless EGL takes the `virgl` driver on it instead of
# falling back to `softpipe`. That path exists because libdrm is built as its
# OpenBSD/DragonFly branch and the render node answers `DRM_IOCTL_GET_PCIINFO`
# (`.notes/wayland.md` §5 row 24): the loader reads the virtio-gpu ids from it and
# picks `virtio_gpu`. `FEED_PACE` because a GL display backend drops serial bytes
# mid-line (see `test-drmgl-x86`); the assertion is the scenario's
# `gltriangle: pass`, which is a real readback.
test-gltriangle-virgl-x86 boot-timeout="180": gltriangle-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/gltriangle.tsv sh tools/smoke/feed.sh target/test-gltriangle-virgl-x86.log {{boot-timeout}} qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-gl-pci,blob=on
    @just _assert-qemu-log target/test-gltriangle-virgl-x86.log "gltriangle: pass"
    @just _assert-qemu-log target/test-gltriangle-virgl-x86.log "gltriangle: GL_RENDERER=virgl"
    @echo "gltriangle-virgl: the GLES2 triangle rendered through surfaceless EGL on the render node, via virgl (x86_64)"

# wlgl: build /bin/wlgl, the GL present client (§6.10 stage 3d-a). It is the tree's
# first *dynamically linked Rust* program, and it has to be one: `dlopen` needs a
# loader in the address space and a statically linked minix image has none
# (`crates/minix-libc/src/lib.rs`), so a program that loads Mesa must be dynamic.
# `tools/build-wlgl.py` is the link: the arch's C crt0 for `_start`, `minix-user.ld`,
# and Mesa's headers for the GL half (`tools/wlgl_render.c`).
wlgl-x86: userland-x86 dynlib-x86
    python tools/build-mesa.py x86 --stage
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'wlgl: no Mesa DSOs — run `just build-mesa x86 --build` first' >&2; exit 1)
    python tools/build-wlgl.py x86
    @echo "wlgl: /bin/wlgl built and the Mesa DSOs staged (x86_64)"

# The 3d-a gate: the client renders a GL surface with Mesa on the render node and
# commits it through /sbin/wlserver to /dev/fb, then reads the composited frame back.
# `GL_RENDERER=virgl` is asserted too, so the frame is the host GPU's and not the
# software fallback's. It is a dev-host gate like 3c-3 — headless CI has no GL.
test-wlgl-x86 boot-timeout="60": wlgl-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/bin/wlgl=$M/bin/wlgl"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/wlgl.tsv sh tools/smoke/feed.sh target/test-wlgl-x86.log {{boot-timeout}} qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-gl-pci,blob=on
    @just _assert-qemu-log target/test-wlgl-x86.log "wlgl: GL_RENDERER=virgl"
    @just _assert-qemu-log target/test-wlgl-x86.log "wlgl: PASS"
    @echo "wlgl: a GL surface rendered on the render node, committed through the compositor, and read back from /dev/fb (x86_64)"

# gbmtest: build /bin/gbmtest, the GBM create/export probe (§6.10 stage 3d-b-3). It links
# `libgbm` and `libdrm` and includes the GBM/libdrm headers from the source checkouts, so it
# is built from `just build-mesa x86 --build` like the DSOs are.
gbmtest-x86: userland-x86 dynlib-x86
    python tools/build-mesa.py x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'gbmtest: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    python tools/build-gbmtest.py x86
    @echo "gbmtest: /bin/gbmtest built and the Mesa DSOs staged (x86_64)"

# The 3d-b-3 gate: a GBM buffer object made on the render node, exported to a dma-buf by
# `gbm_bo_get_fd`, and imported back by `drmPrimeFDToHandle` (`.notes/wayland.md` §6.10). It
# is a dev-host gate like 3c-3 and 3d-a — GBM allocates through the node's DRI driver, which
# needs a GL-capable host — so headless CI does not run it. `dri_gbm.so` is the backend
# `libgbm` `dlopen`s, at `/lib/dri_gbm.so` (`gbm-backends-path`).
test-gbmtest-x86 boot-timeout="120": gbmtest-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/gbmtest=$M/bin/gbmtest"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/gbmtest.tsv sh tools/smoke/feed.sh target/test-gbmtest-x86.log {{boot-timeout}} qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0 -device virtio-gpu-gl-pci,blob=on
    @just _assert-qemu-log target/test-gbmtest-x86.log "gbmtest: dev=1 bo=1 w=64 h=64 import=0"
    @echo "gbmtest: a GBM buffer object created on the render node exported to a dma-buf and imported back (x86_64)"

# The 3d-b gate: a whole-output `wl_shm` client is presented by re-pointing the scanout at its own
# buffer, so the compositor copies nothing (§6.10). `/dev/fb` no longer holds the frame (the frame
# *is* the client's pool), so tools/wlfull_probe.py checks the display with a QMP screendump and the
# fb server's `fb: scanout src` line. On x86 the output device is a `virtio-gpu`, not `bochs`: the
# re-point is a virtio-gpu capability (D11); on riscv64/aarch64 the `virtio-gpu` is already the
# output, so only the full-output client is new.
test-wlfull arch="x86":
    @just test-wlfull-{{arch}}

test-wlfull-x86: build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/wlfull_probe.py qemu-system-x86_64 -nographic -monitor none -display none -vga none -device virtio-gpu-pci -m 256M -no-reboot -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlfull: a whole-output wl_shm frame was scanned out of the client's own buffer (x86_64)"

# The 3d-b-4c gate: a linux-dmabuf client's render-node object is scanned out with no copy
# (§6.10). The output is a virtio-gpu with no bochs-display, so the render node is served from
# the output device itself (D11) — the client's render resource and the scanout are one device,
# which is what lets the fb server re-point the scanout at it. A QMP screendump of the client's
# colour and the fb server's `fb: scanout src` line are the evidence.
test-wldmabuf-x86: build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/wldmabuf_probe.py qemu-system-x86_64 -nographic -monitor none -display none -vga none -device virtio-gpu-pci -m 256M -no-reboot -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wldmabuf: a render-node object was handed over as a linux-dmabuf buffer and scanned out with no copy (x86_64)"

# wldmabufgl: build /bin/wldmabufgl, the 3d-b-4d GL present client (§6.10). Like /bin/wlgl it
# is a dynamic image that renders with Mesa, but it draws into the buffer it commits instead
# of reading the frame back: the render target is a GBM object on the render node, turned into
# a GL texture through an `EGLImage` over the object's descriptor, exported, and committed as
# a `zwp_linux_dmabuf_v1` buffer. The EGLImage route is the one Mesa's build here allows —
# `-Dplatforms=` leaves no `EGL_PLATFORM_GBM_KHR` — and `tools/egl_bo_probe.c` is the
# measurement that it works on the port.
wldmabufgl-x86: userland-x86 dynlib-x86
    python tools/build-mesa.py x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'wldmabufgl: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    python tools/build-wldmabufgl.py x86
    @echo "wldmabufgl: /bin/wldmabufgl built and the Mesa DSOs staged (x86_64)"

# The 3d-b-4d gate: the GL client's render target *is* the dma-buf `wl_buffer`, which the
# compositor scans out of the client's own object — no compositor copy. It is a dev-host gate
# like 3c-3/3d-a/3d-b-3 — headless CI has no GL. The QEMU line puts a 2D `virtio-gpu-pci`
# first and the GL device second: the client renders on the GL node and the fb server re-points
# the 2D scanout at the client's object, so the screendump (which follows the first, 2D device)
# shows the frame — a GL device *as* the output would need a host blob this host refuses (D11).
# `-display egl-headless` is what a GL device needs. The one copy on this path is the client's,
# not the compositor's: this host's virgl renders into a host-side texture, so the client
# resolves it and transfers it into the object before committing (`.notes/wayland.md` §5, and
# `tools/egl_bo_probe.c` is the measurement).
test-wldmabufgl-x86 boot-timeout="120": wldmabufgl-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wldmabufgl=$M/bin/wldmabufgl"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wldmabufgl_probe.py qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device virtio-gpu-pci -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-gpu-gl-pci,blob=on -qmp tcp:127.0.0.1:4447,server=on,wait=off
    @echo "wldmabufgl: the GL triangle's render target was the dma-buf wl_buffer, scanned out of the client's object with no compositor copy (x86_64)"

# wlcompose: build /bin/wlcompose, the 3d-b-5b GPU-composition helper (§6.10). Like the other Mesa
# gates it is a dynamic image; one binary is both the helper and its test peer, selected by argv
# (`serve`/`test`), which is what makes the cross-process measurement a real two-process one.
wlcompose-x86: userland-x86 dynlib-x86
    python tools/build-mesa.py x86 --stage
    @test -f target/mesa/x86/lib/libgbm.so.1 || (echo 'wlcompose: no libgbm — run `just build-mesa x86 --build` first' >&2; exit 1)
    python tools/build-wlcompose.py x86
    @echo "wlcompose: /bin/wlcompose built and the Mesa DSOs staged (x86_64)"

# The 3d-b-5b gate: two GL surfaces one process rendered are composed on the render node by a
# helper in *another* process, and the peer reads the composed frame back. It is a dev-host gate
# like 3c-3/3d-a/3d-b-4d — headless CI has no GL. The peer only passes if it read the peer-drawn
# colours out of the returned frame, so this measures cross-process sampling, not merely a crash.
# The devices are the 4d gate's — a 2D `virtio-gpu-pci` and a GL device — the pair the port's role
# selection (D11) has been validated with; `-display egl-headless` is what a GL device needs.
test-wlcompose-x86 boot-timeout="120": wlcompose-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wlcompose=$M/bin/wlcompose"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlcompose_probe.py qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device virtio-gpu-pci -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-gpu-gl-pci,blob=on
    @echo "wlcompose: two GL surfaces were composed on the render node by a helper in another process, and the frame read back holds them (x86_64)"

# The 3d-b-5c/5d gate: two dma-buf surfaces committed on one connection are composed on the GPU by
# the helper process and scanned out as one frame. `/bin/wlscene` is a plain static client; the
# helper is the dynamic `/bin/wlcompose`, injected with the Mesa DSOs. The devices and the GL
# display are the 5b gate's, plus QMP so the host can read the display the frame lands on.
test-wlscene-x86 boot-timeout="120": wlcompose-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/bin/wlcompose=$M/bin/wlcompose"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlscene_probe.py qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device virtio-gpu-pci -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-gpu-gl-pci,blob=on -qmp tcp:127.0.0.1:4447,server=on,wait=off
    @echo "wlscene: the compositor composed two dma-buf surfaces on the GPU and scanned the frame out (x86_64)"

test-wlfull-riscv64: system-image-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    python tools/wlfull_probe.py qemu-system-riscv64 -machine virt -m 256M -nographic -monitor none -display none -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf -qmp tcp:127.0.0.1:4447,server=on,wait=off
    @echo "wlfull: a whole-output wl_shm frame was scanned out of the client's own buffer (riscv64)"

test-wlfull-aarch64: system-image-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    python tools/wlfull_probe.py qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -monitor none -display none -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64-boot.elf -qmp tcp:127.0.0.1:4447,server=on,wait=off
    @echo "wlfull: a whole-output wl_shm frame was scanned out of the client's own buffer (aarch64)"

# Phase 4a's gate (`.notes/wayland.md` 6.13): calloop's event loop on the port, which is the
# loop smithay and every compositor above it live on. Four sources — a calloop timer, a timerfd,
# an eventfd and a pipe — must all dispatch through one poll in one process. The binary needs
# the forked OS crates (calloop, polling, rustix, libc), so it comes from its own cargo project
# and is injected with MINIXFS_EXTRA rather than being a BOOT_BINS entry.
# The gate binary itself, from its own cargo project (`tools/build-phase4-gate.py`): the forks
# it links are patched in that project, not in this workspace.
calloop-x86:
    python tools/build-phase4-gate.py x86

calloop-riscv64:
    python tools/build-phase4-gate.py riscv64

calloop-aarch64:
    python tools/build-phase4-gate.py aarch64

test-calloop-x86 boot-timeout="40": calloop-x86
    MINIXFS_EXTRA='/bin/calloopgate=target/x86_64-pc-minix/release/calloopgate' just build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/calloop.tsv sh tools/smoke/feed.sh target/test-calloop-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "calloop: a timer, a timerfd, an eventfd and a pipe dispatched through one event loop (x86_64)"

test-calloop-riscv64 boot-timeout="60": calloop-riscv64
    MINIXFS_EXTRA='/bin/calloopgate=target/riscv64gc-unknown-minix/release/calloopgate' just build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/calloop.tsv sh tools/smoke/feed.sh target/test-calloop-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "calloop: a timer, a timerfd, an eventfd and a pipe dispatched through one event loop (riscv64)"

test-calloop-aarch64 boot-timeout="60": calloop-aarch64
    MINIXFS_EXTRA='/bin/calloopgate=target/aarch64-unknown-minix/release/calloopgate' just build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/calloop.tsv sh tools/smoke/feed.sh target/test-calloop-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "calloop: a timer, a timerfd, an eventfd and a pipe dispatched through one event loop (aarch64)"

# Phase 4c's gate (`.notes/wayland.md` 6.13): wayland-rs's *server runtime* — a `Display`, a
# `GlobalDispatch` global and the request `Dispatch` impls — serving a stock `wayland-client`
# over the port's `/dev/uds` socket. `udsprobe` rides in the same scenario: it walks the accept
# path (the driver's ioctls, then `std`'s wrapper) and the C ABI's `send` flags one step at a
# time, so a failure names the primitive instead of the server.
wlgate-x86:
    python tools/build-phase4-gate.py x86

wlgate-riscv64:
    python tools/build-phase4-gate.py riscv64

wlgate-aarch64:
    python tools/build-phase4-gate.py aarch64

test-wlgate-x86 boot-timeout="40": wlgate-x86
    MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA MINIXFS_EXTRA='/bin/wlgate=target/x86_64-pc-minix/release/wlgate;/bin/udsprobe=target/x86_64-pc-minix/release/udsprobe' just build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/wlgate.tsv sh tools/smoke/feed.sh target/test-wlgate-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wlgate: wayland-server served a stock client over the port's socket (x86_64)"

test-wlgate-riscv64 boot-timeout="60": wlgate-riscv64
    MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA MINIXFS_EXTRA='/bin/wlgate=target/riscv64gc-unknown-minix/release/wlgate;/bin/udsprobe=target/riscv64gc-unknown-minix/release/udsprobe' just build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/wlgate.tsv sh tools/smoke/feed.sh target/test-wlgate-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "wlgate: wayland-server served a stock client over the port's socket (riscv64)"

test-wlgate-aarch64 boot-timeout="60": wlgate-aarch64
    MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA MINIXFS_EXTRA='/bin/wlgate=target/aarch64-unknown-minix/release/wlgate;/bin/udsprobe=target/aarch64-unknown-minix/release/udsprobe' just build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/wlgate.tsv sh tools/smoke/feed.sh target/test-wlgate-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "wlgate: wayland-server served a stock client over the port's socket (aarch64)"

# Phase 4d-1's gate (`.notes/wayland.md` 6.13): smithay's own GL path on the port -
# a surfaceless EGL display, a GLES 3 context, `GlesRenderer`, an offscreen target, a clear and
# the read-back. It is the renderer 4d composes with (D17/D18), and it is the tree's first
# *dynamically linked* Rust binary that uses std: `GlesRenderer` loads EGL with `dlopen`, and a
# static image has no loader. `libxkbcommon` is linked in (statically - see
# `tools/build-xkbcommon.py`).
#
# No GL device on the QEMU line, on purpose: a GL device needs a display backend with GL, which a
# CI runner has not, and this gate is meant to boot anywhere. The software driver is what runs and
# the pixel it reads back is the assertion. That is not a claim about virgl: with
# `-device virtio-gpu-gl-pci,blob=on` the guest gets virgl, OpenGL ES 3.2 and both dma-buf import
# extensions, and `ExportMem` works there; without `blob=on` it stays on softpipe instead.
glgate-x86: dynlib-x86
    python tools/build-xkbcommon.py x86 --build
    python tools/build-phase4-comp.py x86
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'glgate: no Mesa DSOs - run `just fetch-mesa x86` or `just build-mesa x86 --build` first' >&2; exit 1)
    @echo "glgate: /bin/glgate built and linked against libc.so (x86_64)"

test-glgate-x86 boot-timeout="60": glgate-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/glgate=target/x86_64-pc-minix/release/glgate"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    FEED_PACE=0.02 FEED_SCENARIO=tools/smoke/glgate.tsv sh tools/smoke/feed.sh target/test-glgate-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @just _assert-qemu-log target/test-glgate-x86.log "glgate: PASS clear=green"
    @echo "glgate: smithay's GlesRenderer ran on the port and read its clear back (x86_64)"

# Phase 4d-2's gate (`.notes/wayland.md` 6.13): smithay's `CompositorState` and `ShmState` with a
# `wl_output`, serving a stock `wayland-client` that commits two whole-output `wl_shm` frames. The
# compositor draws into the offscreen GLES target 4d-1 proved, reads the frame back (D18) and
# re-points the fb server's scanout at it (3d-b-1), so the display shows what smithay composed.
#
# The output is a `virtio-gpu-pci` and not a bochs display: only a virtio-gpu can re-back its
# scanout (D11). The probe takes a QMP screendump (the client's second frame's colour) and reads
# the serial log for `fb: scanout src`, the compositor's present line and the client's buffer
# release, which is the compositor-side half of the gate.
wlcomp-x86: dynlib-x86
    python tools/build-xkbcommon.py x86 --build
    python tools/build-phase4-comp.py x86
    @test -f target/mesa/x86/lib/libEGL.so.1 || (echo 'wlcomp: no Mesa DSOs - run `just fetch-mesa x86` or `just build-mesa x86 --build` first' >&2; exit 1)
    @echo "wlcomp: /bin/wlcomp and /bin/wlshm built and linked against libc.so (x86_64)"

test-wlcomp arch="x86":
    @just test-wlcomp-{{arch}}

test-wlcomp-x86: wlcomp-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/wlcomp=target/x86_64-pc-minix/release/wlcomp;/bin/wlshm=target/x86_64-pc-minix/release/wlshm"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlcomp_probe.py qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device virtio-gpu-pci -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlcomp: smithay composed a stock wl_shm client onto the scan-out (x86_64)"

# The same compositor gate with the software renderer selected: `/bin/wlcomp soft` draws the
# committed surfaces with this project's own rasteriser and never opens EGL, so the frame the
# scan-out gets came from no GL driver at all. The probe requires `wlcomp: renderer soft` in
# the log, which is what keeps the GL path's fallback from passing for it.
test-wlcomp-soft-x86: wlcomp-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    MINIXFS_EXTRA="/lib/libdrm.so.2=target/mesa/x86/lib/libdrm.so.2;/lib/libEGL.so.1=target/mesa/x86/lib/libEGL.so.1;/lib/libgbm.so.1=target/mesa/x86/lib/libgbm.so.1;/lib/libexpat.so.1=target/mesa/x86/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=target/mesa/x86/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=target/mesa/x86/lib/libGLESv2.so.2;/lib/libz.so.1=target/mesa/x86/lib/libz.so.1;/lib/dri_gbm.so=target/mesa/x86/lib/dri_gbm.so;/lib/swrast_dri.so=target/mesa/x86/lib/swrast_dri.so;/bin/wlcomp=target/x86_64-pc-minix/release/wlcomp;/bin/wlshm=target/x86_64-pc-minix/release/wlshm" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlcomp_probe.py --renderer soft qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device virtio-gpu-pci -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlcomp: the software renderer composed a stock wl_shm client onto the scan-out (x86_64)"

# The dma-buf gate (`.notes/wayland.md` 6.13): a stock `wayland-client` commits
# `zwp_linux_dmabuf_v1` buffers that are render-node objects, so the pixels the compositor draws
# are the client's own object (`tools/phase4-comp/src/dmabuf.rs`), with no frame copied between
# them. It runs with the software renderer, which is the one that can import on a device set with
# no GL device: it maps the descriptor through the node. The probe requires
# `wlcomp: dmabuf imported by the soft renderer`.
test-wlcomp-dmabuf-x86: wlcomp-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    MINIXFS_EXTRA="/lib/libdrm.so.2=target/mesa/x86/lib/libdrm.so.2;/lib/libEGL.so.1=target/mesa/x86/lib/libEGL.so.1;/lib/libgbm.so.1=target/mesa/x86/lib/libgbm.so.1;/lib/libexpat.so.1=target/mesa/x86/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=target/mesa/x86/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=target/mesa/x86/lib/libGLESv2.so.2;/lib/libz.so.1=target/mesa/x86/lib/libz.so.1;/lib/dri_gbm.so=target/mesa/x86/lib/dri_gbm.so;/lib/swrast_dri.so=target/mesa/x86/lib/swrast_dri.so;/bin/wlcomp=target/x86_64-pc-minix/release/wlcomp;/bin/wlshm=target/x86_64-pc-minix/release/wlshm;/bin/wldmabuf=target/x86_64-pc-minix/release/wldmabuf" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlcomp_probe.py --renderer soft --client dmabuf qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device virtio-gpu-pci -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlcomp: a dma-buf client's frame reached the scan-out through the software renderer (x86_64)"

# The same dma-buf client on the GL renderer, whose import is an EGLImage: the compositor samples
# the client's object through virgl, so this is the gate for the path a real compositor takes.
# It needs a host GL that gives the guest virgl ES 3, which here means Mesa (WSL): Windows' ANGLE
# offers ES2-only configs, so the compositor falls back to its software renderer and the probe
# fails at `wlcomp: renderer gl`.
# The device set is `/bin/wlcompose`'s: a 2D `virtio-gpu-pci` first, which is the display the
# `fb: scanout src` re-point and the screendump need, and the GL device second, which EGL picks
# because the 2D one has no GL. The probe requires `wlcomp: dmabuf imported by the gl renderer`
# and the client's colours in the screendump.
test-wlcomp-dmabuf-gl-x86: wlcomp-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    MINIXFS_EXTRA="/lib/libdrm.so.2=target/mesa/x86/lib/libdrm.so.2;/lib/libEGL.so.1=target/mesa/x86/lib/libEGL.so.1;/lib/libgbm.so.1=target/mesa/x86/lib/libgbm.so.1;/lib/libexpat.so.1=target/mesa/x86/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=target/mesa/x86/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=target/mesa/x86/lib/libGLESv2.so.2;/lib/libz.so.1=target/mesa/x86/lib/libz.so.1;/lib/dri_gbm.so=target/mesa/x86/lib/dri_gbm.so;/bin/wlcomp=target/x86_64-pc-minix/release/wlcomp;/bin/wlshm=target/x86_64-pc-minix/release/wlshm;/bin/wldmabuf=target/x86_64-pc-minix/release/wldmabuf" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlcomp_probe.py --renderer gl --client dmabuf qemu-system-x86_64 -display egl-headless -serial stdio -m 512M -no-reboot -vga none -device virtio-gpu-pci -device virtio-gpu-gl-pci,blob=on -qmp tcp:127.0.0.1:4447,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlcomp: a dma-buf client's frame reached the scan-out through the GL renderer (x86_64)"

# Phase 4d-3's gate (`.notes/wayland.md` 6.13): a `wl_seat` with a keyboard on the smithay
# compositor, fed from the input server's ring through the port's own `InputBackend`. The host
# injects `a` with QMP -- the serial console never reaches the input server -- and the stock
# client must read the keymap the seat serves and then receive evdev 30.
#
# The seat needs an xkb keymap, and smithay compiles one from xkb components, which this image
# does not install; the compositor materialises the components from the compiled keymap the
# in-house server serves (`crates/servers/data/us-evdev.xkb`) under `/tmp/xkb` at startup.
wlseat-x86: wlcomp-x86

test-wlseat-x86: wlseat-x86 boot-image-x86
    @just _assert-qemu-version qemu-system-x86_64
    M=target/mesa/x86; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/dri_gbm.so=$M/lib/dri_gbm.so;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/wlcomp=target/x86_64-pc-minix/release/wlcomp;/bin/wlshm=target/x86_64-pc-minix/release/wlshm;/bin/wlseat=target/x86_64-pc-minix/release/wlseat"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- x86_64
    @just mkfs-x86
    python tools/wlseat_probe.py qemu-system-x86_64 -nographic -m 512M -no-reboot -vga none -device virtio-gpu-pci -qmp tcp:127.0.0.1:4472,server=on,wait=off -kernel target/images/x86_64-pc-minix/minix-x86-boot.elf -drive if=none,id=disk0,file=target/images/x86_64-pc-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0
    @echo "wlseat: a host key reached a stock client through smithay's seat (x86_64)"

test-wlseat arch="x86":
    @just test-wlseat-{{arch}}

test-gltriangle-riscv64 boot-timeout="180": gltriangle-riscv64 boot-image-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    M=target/mesa/riscv64; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- riscv64
    @just mkfs-riscv64
    FEED_SCENARIO=tools/smoke/gltriangle.tsv sh tools/smoke/feed.sh target/test-gltriangle-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 512M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/riscv64gc-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/riscv64gc-unknown-minix/minix-riscv64-boot.elf
    @echo "gltriangle: the GLES2 triangle rendered through surfaceless EGL on softpipe (riscv64)"

test-gltriangle-aarch64 boot-timeout="180": gltriangle-aarch64 boot-image-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    M=target/mesa/aarch64; E="/lib/libdrm.so.2=$M/lib/libdrm.so.2;/lib/libEGL.so.1=$M/lib/libEGL.so.1;/lib/libgbm.so.1=$M/lib/libgbm.so.1;/lib/libexpat.so.1=$M/lib/libexpat.so.1;/lib/libgallium-25.3.6.so=$M/lib/libgallium-25.3.6.so;/lib/libGLESv2.so.2=$M/lib/libGLESv2.so.2;/lib/libz.so.1=$M/lib/libz.so.1;/lib/swrast_dri.so=$M/lib/swrast_dri.so;/bin/gltriangle=$M/bin/gltriangle"; MINIXFS_EXTRA="$E" MINIXFS_BLOCKS=16384 cargo run -q -p boot-image --bin mkminixfs --release -- aarch64
    @just mkfs-aarch64
    FEED_SCENARIO=tools/smoke/gltriangle.tsv sh tools/smoke/feed.sh target/test-gltriangle-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 512M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/images/aarch64-unknown-minix/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/images/aarch64-unknown-minix/minix-aarch64-boot.elf
    @echo "gltriangle: the GLES2 triangle rendered through surfaceless EGL on softpipe (aarch64)"

# UNIX-domain sockets: /bin/udstest round-trips a socketpair and a
# bind/listen/connect/accept connection through the /dev/uds server — Phase 0 of
# .notes/wayland.md. build-x86 embeds the binary; the scenario types the command into
# the shell and wants the whole line back.
test-uds-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/uds.tsv sh tools/smoke/feed.sh target/test-uds-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "uds: socketpair, bind/listen/connect/accept and SCM_RIGHTS/SCM_CREDS passing (x86_64)"

test-memfd-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/memfd.tsv sh tools/smoke/feed.sh target/test-memfd-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "memfd: one object mapped twice and across a fork, read/write agreeing (x86_64)"

# The terminal path's readiness: /bin/ptytest opens a pty master, forks a slave,
# and must come back from a `poll` on the master when the slave writes. Nothing
# woke a master poller before the tty server reported it, which is why /bin/wterm
# drained the master in an EAGAIN loop instead of polling. build-x86 embeds the
# binary.
test-pty-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/pty.tsv sh tools/smoke/feed.sh target/test-pty-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "pty: a poll on a pty master is woken by the slave's write (x86_64)"

# Readiness primitives: /bin/seltest drives select(2) and poll(2) over pipes and
# checks a real timeout on each — Phase 0 of .notes/wayland.md. build-x86 embeds the
# binary; the scenario types the command into the shell and wants the whole line back.
test-select-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/select.tsv sh tools/smoke/feed.sh target/test-select-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "select/poll: pipe readiness and real timeouts (x86_64)"

# eventfd: /bin/eventfdtest exercises the counter semantics and a cross-process
# wake of a blocked poll — Phase 0 of .notes/wayland.md. build-x86 embeds the binary.
test-eventfd-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/eventfd.tsv sh tools/smoke/feed.sh target/test-eventfd-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "eventfd: counter semantics and a cross-process wake (x86_64)"

# timerfd: /bin/timerfdtest drives one-shot, periodic and absolute timers (and a
# disarm) through blocking polls — Phase 0 of .notes/wayland.md.
test-timerfd-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/timerfd.tsv sh tools/smoke/feed.sh target/test-timerfd-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "timerfd: one-shot, periodic, absolute and disarmed timers (x86_64)"

# epoll: /bin/epolltest drives a persistent interest set — add/modify/remove,
# level-triggered readiness over an eventfd and a pipe, and a blocked
# `epoll_wait` woken by a child process — Phase 0 of .notes/wayland.md.
test-epoll-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/epoll.tsv sh tools/smoke/feed.sh target/test-epoll-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "epoll: persistent interest set, level-triggered readiness, cross-process wake (x86_64)"

# wayland: /bin/waylandtest drives the Phase 1a protocol handshake (registry and
# wl_display.sync) between a forked client and server over /dev/uds — .notes/wayland.md
# §6.11. build-x86 embeds the binary.
test-wayland-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/wayland.tsv sh tools/smoke/feed.sh target/test-wayland-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wayland: registry + sync handshake over /dev/uds (x86_64)"

# wlshm: /bin/wlclient drives the Phase 1b wl_shm present path — it draws into a
# memfd pool, passes the fd to /sbin/wlserver (boot proc 20), commits a surface,
# and reads the composited frame back from /dev/fb — .notes/wayland.md §6.11. build-x86
# embeds both binaries.
test-wlshm-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/wlshm.tsv sh tools/smoke/feed.sh target/test-wlshm-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wlshm: wl_shm buffer committed and presented to /dev/fb (x86_64)"

# wlkey: tools/wlkey_probe.py drives the Phase 1c input path and the Phase 2a
# keymap — /bin/wlkey binds wl_seat's keyboard, resolves the `keymap` event's fd
# and checks its bytes (.notes/wayland.md §6.12), commits a surface, and a key injected
# with QMP reaches it as `wl_keyboard.key` — .notes/wayland.md §6.11. A host probe rather
# than a tsv scenario: the key has to be a real device event, and the serial console
# the tsv gates type into is not one.
test-wlkey-x86: build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/wlkey_probe.py
    @echo "wlkey: keymap served and wl_seat keyboard events reach a client (x86_64)"


# wlx: /bin/wlx drives the Phase 2b xdg_shell path — it maps an xdg_toplevel, the
# server sends `configure`, the client acks it and commits a frame of the configured
# size, and the frame reaches /dev/fb — .notes/wayland.md §6.12.
test-wlx-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/wlx.tsv sh tools/smoke/feed.sh target/test-wlx-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wlx: xdg_shell toplevel configured, acked and presented (x86_64)"


# wlfocus: tools/wlfocus_probe.py drives the Phase 2c focus path — /bin/wlx2 opens
# two connections, maps a window on each, and a QMP key must reach the focused one as
# the focus moves between them — .notes/wayland.md §6.12. A host probe, like wlkey's: a key
# has to be a real device event, which the serial console is not.
test-wlfocus-x86: build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/wlfocus_probe.py
    @echo "wlfocus: two clients, one focus, and the keys follow it (x86_64)"


# wlxd: /bin/wlxd drives the Phase 2d damage and cursor path — it damages a small
# rectangle and checks the rest of the frame survives, then gives the pointer an
# image and finds it on /dev/fb — .notes/wayland.md §6.12.
test-wlxd-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/wlxd.tsv sh tools/smoke/feed.sh target/test-wlxd-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wlxd: damage and the pointer's cursor (x86_64)"


# wlxe: /bin/wlxe drives the Phase 2e panel, popup and decoration path — it maps an
# opaque window, then a zwlr_layer_shell_v1 panel and an xdg_popup over it, requires
# the decoration manager to answer client-side, and checks both are composited where
# they asked and no wider — .notes/wayland.md §6.12.
test-wlxe-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/wlxe.tsv sh tools/smoke/feed.sh target/test-wlxe-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "wlxe: layer-shell panel, xdg_popup and client-side decoration (x86_64)"
# Measure whether two processes that map one object through the loader share its
# physical frames — Phase 5 of .notes/dynamic_linking.md. `tools/dso_share_probe.py` boots
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
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    python tools/dso_share_probe.py --arch x86

probe-dso-share-riscv64: dynlink-riscv64
    DYNLINK_BINS='/lib/libdyn.so=target/dynlink/riscv64/libdyn.so;/lib/libdyn2.so=target/dynlink/riscv64/libdyn2.so;/lib/libdyn3.so=target/dynlink/riscv64/libdyn3.so;/bin/dynhello=target/dynlink/riscv64/dynhello' just build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    python tools/dso_share_probe.py --arch riscv64

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

build-riscv64-test: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-test --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,integration-tests --release

build-aarch64-test: userland-aarch64 dynlib-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64-test --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,integration-tests --release

build-riscv64-boot: userland-riscv64 coreutils-riscv64 dynlib-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64-boot --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,boot-test --release

build-aarch64-boot: userland-aarch64 dynlib-aarch64
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

# The per-arch QEMU gates a change to a VA layout, a HAL constant or anything arch-gated
# has to be followed by. They cover different things and none substitutes for another:
# `test-qemu` runs the in-kernel suite (`crates/kernel/src/tests.rs`, behind the
# `qemu-tests` feature), `test-boot` boots to a shell and runs `boot_test.rs`, and
# `test-fork` forks a process and checks both directions of the copy. The host suite
# cannot stand in for the first, because `tests.rs` is compiled only under `qemu-tests` -
# which is exactly how `syscall_brk` came to assert x86_64's heap base and fail on aarch64
# alone: `cargo test` never saw it.
#
# Grouped by arch so each arch's userland/coreutils build is reused, and stops at
# the first failing gate like the individual recipes (the failing log is on
# stdout).
#
# All twelve QEMU gates in one command.
test-arches:
    @just test-qemu x86
    @just test-boot x86
    @just test-fork x86
    @just test-waitid x86
    @just test-cabi x86
    @just test-winprobe x86
    @just test-qemu riscv64
    @just test-boot riscv64
    @just test-fork riscv64
    @just test-waitid riscv64
    @just test-winprobe riscv64
    @just test-qemu aarch64
    @just test-boot aarch64
    @just test-fork aarch64
    @just test-waitid aarch64
    @just test-winprobe aarch64

# Fork isolation, per arch: `tools/smoke/fork.tsv` runs `/bin/forktest`, which forks and checks
# both directions of the copy - the child's write must not reach the parent, and the parent's
# post-fork write must not reach a child that only reads that page. The second direction is what
# catches a fork sharing frames instead of copying them, and it is per arch because each arch's
# `vm_paging_fork` builds the COW mapping itself (aarch64's AP[2:1] encoding is its own).
#
# `/bin/forktest` is a `BOOT_BINS` entry, so every image carries it, and this is the gate the
# per-arch fork entries in `KNOWN_ISSUES.md` point at.
test-fork arch="x86" boot-timeout="40":
    @just test-fork-{{arch}} {{boot-timeout}}

# The user-window alias probe, per arch: `tools/smoke/winprobe.tsv` runs `/bin/winprobe`, which
# asks whether a VA the process was never given is reachable. The invariant is that it is not -
# `KNOWN_ISSUES.md` item 16 - and it is per arch because the fresh exec root is built per arch:
# aarch64 used to fill the window with EL0-RW blocks aliasing the kernel's physical allocator
# (item 16, fixed 2026-10-02), RISC-V does not copy the window's entry at all, and x86's
# identity map stops at the user base. All three answer `reachable=0` now.
test-winprobe arch="x86" boot-timeout="40":
    @just test-winprobe-{{arch}} {{boot-timeout}}

test-winprobe-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/winprobe.tsv sh tools/smoke/feed.sh target/test-winprobe-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "winprobe: no user VA outside a region is reachable (x86_64)"

test-winprobe-riscv64 boot-timeout="60": build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/winprobe.tsv sh tools/smoke/feed.sh target/test-winprobe-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "winprobe: no user VA outside a region is reachable (riscv64)"

test-winprobe-aarch64 boot-timeout="60": build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/winprobe.tsv sh tools/smoke/feed.sh target/test-winprobe-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "winprobe: no user VA outside a region is reachable (aarch64)"

test-fork-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/fork.tsv sh tools/smoke/feed.sh target/test-fork-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "fork: both directions of the copy held (x86_64)"

test-fork-riscv64 boot-timeout="60": build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/fork.tsv sh tools/smoke/feed.sh target/test-fork-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "fork: both directions of the copy held (riscv64)"

test-fork-aarch64 boot-timeout="60": build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/fork.tsv sh tools/smoke/feed.sh target/test-fork-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "fork: both directions of the copy held (aarch64)"

# `waitid` decoding, per arch: `tools/smoke/waitid.tsv` runs `/bin/waittest`, which forks a
# child that exits 42 and checks that `waitid` reports `CLD_EXITED` with `si_status == 42`, that
# `P_ALL` names the child it took, and that `WSTOPPED`/`WNOWAIT`/an empty option set are
# refused. The encoding is PM's and therefore arch-independent, but the gate runs per arch for
# the same reason `test-fork` does: it forks, and an arch whose fork path is broken should fail
# here rather than only in the gate that happens to run on it.
test-waitid arch="x86" boot-timeout="40":
    @just test-waitid-{{arch}} {{boot-timeout}}

test-waitid-x86 boot-timeout="40": build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/waitid.tsv sh tools/smoke/feed.sh target/test-waitid-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "waitid: status decoded, P_ALL named the child, unsupported states refused (x86_64)"

test-waitid-riscv64 boot-timeout="60": build-riscv64
    @just _assert-qemu-version qemu-system-riscv64
    FEED_SCENARIO=tools/smoke/waitid.tsv sh tools/smoke/feed.sh target/test-waitid-riscv64.log {{boot-timeout}} qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64
    @echo "waitid: status decoded, P_ALL named the child, unsupported states refused (riscv64)"

test-waitid-aarch64 boot-timeout="60": build-aarch64
    @just _assert-qemu-version qemu-system-aarch64
    FEED_SCENARIO=tools/smoke/waitid.tsv sh tools/smoke/feed.sh target/test-waitid-aarch64.log {{boot-timeout}} qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -netdev user,id=net0 -device virtio-net-device,netdev=net0 -device virtio-gpu-device -device virtio-keyboard-device -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64
    @echo "waitid: status decoded, P_ALL named the child, unsupported states refused (aarch64)"

# The C libc ABI, x86 only: `tools/build-c-hello.py` builds `/bin/cabi` for x86_64
# alone. `tools/smoke/cabi.tsv` asserts the `sockaddr_in` round trip (the only place
# the header's layout and `minix-libc`'s decoder meet), `pipe2(O_NONBLOCK)` reaching
# the pipe's read path, and an anonymous `mmap`.
test-cabi arch="x86" boot-timeout="40":
    @just test-cabi-{{arch}} {{boot-timeout}}

test-cabi-x86 boot-timeout="40": (build-c-hello "x86") build-x86
    mkdir -p target/images/x86_64-pc-minix
    cp target/trampoline.elf target/images/x86_64-pc-minix/minix-x86.elf
    @just _assert-qemu-version qemu-system-x86_64
    FEED_SCENARIO=tools/smoke/cabi.tsv sh tools/smoke/feed.sh target/test-cabi-x86.log {{boot-timeout}} qemu-system-x86_64 -nographic -m 256M -no-reboot -vga none -device bochs-display,id=fb0 -kernel target/images/x86_64-pc-minix/minix-x86.elf -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0 -device virtio-tablet-pci,display=fb0
    @echo "cabi: the C socket, pipe2 and mmap ABIs held (x86_64)"

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
# boots it. `.notes/c_build.md` has the why of each flag and each trap this build costs.
#
# Run this *after* the toolchain: `bootstrap` and `fetch-stage1` start with
# `cargo clean`, which takes `target/bash-src` and the build tree with it.
build-bash arch="x86":
    python tools/build-bash.py {{arch}}

# Install the prebuilt bash for the pinned commit (`tools/bash_pin.py`) instead of
# building it: the release `.github/workflows/bash-release.yml` publishes, tagged
# by that commit. `just build-bash` stays authoritative (a C header or libc change
# needs it); this is the cache CI and dev machines use.
fetch-bash arch="x86" *args:
    python tools/fetch-bash.py {{arch}} {{args}}

# Create or refresh the `sdgoij/bash` GitHub mirror and push it, so the build never
# clones savannah (it is slow and times out) — `tools/mirror-bash.py` takes the
# objects from upstream, verifies the pinned commit (`tools/bash_pin.py`) is present,
# and pushes. Needs an empty `sdgoij/bash` in the org and a key that can push to it.
# `just mirror-bash --check` only verifies the pin, without pushing.
mirror-bash *args:
    python tools/mirror-bash.py {{args}}

# Fetch and configure Mesa + libdrm for a target (§6.10 stage 3c-0): pinned sources
# under `target/`, a meson cross file naming `tools/cc-dso-minix.py`, and
# `meson setup` for the softpipe branch. Meson and ninja must be on the host's
# PATH (a venv install is enough — see the script).
build-mesa arch="x86" *args:
    python tools/build-mesa.py {{arch}} {{args}}

# Install the prebuilt Mesa + libdrm DSOs for the pinned version
# (`tools/mesa_pin.py`) instead of building them: the release
# `.github/workflows/mesa-release.yml` publishes, tagged by the pinned Mesa and
# libdrm commits. `just build-mesa` stays authoritative (it is what a pin bump or
# a `cc-dso-minix.py` change needs); this is the cache CI and dev machines use.
fetch-mesa arch="x86" *args:
    python tools/fetch-mesa.py {{arch}} {{args}}

# libxkbcommon, the C library smithay's `xkbcommon` binding links with. rustc emits a
# crate's native link flags whether or not its symbols are reachable, so any binary in
# smithay's graph needs `-lxkbcommon` — 4d cannot link a compositor without it
# (`.notes/wayland.md` 6.13). `--build` configures and compiles it in WSL (meson's build
# tree wants a native filesystem) and stages `libxkbcommon.so` (link time) and
# `libxkbcommon.so.0` (the guest's `DT_NEEDED`) under `target/xkbcommon/<arch>/lib`; with
# no flag it only stages.
xkbcommon arch="x86" *args:
    python tools/build-xkbcommon.py {{arch}} {{args}}

# The dynamic-linking artifacts an *image* carries: the loader (`/libexec/ld.so`,
# `crates/ldso` linked with its own script, `tools/minix-ldso.ld`), the shared C library
# (`/lib/libc.so`) and a C program linked against it (`/bin/dynclib`). All three are in
# `crates/boot-image/src/manifest.rs`'s `BOOT_BINS`, so every recipe that assembles an
# image depends on this one, and an image has dynamic linking whether or not its gate was
# ever run. The loader is what a `PT_INTERP` binary needs to be exec'd at all; nothing in
# the boot path carries one, so an image with these three boots exactly as before
# (`.notes/dynamic_linking.md` D2).
#
# The loader lands in the release dir from cargo, and `tools/build-dynlibc.py` puts the
# other two there, the way `tools/build-c-hello.py` does for helloc/ctest.
dynlib-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found - run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-ldso.ld -C link-arg=--no-eh-frame-hdr" cargo build -p ldso --bin ldso --features bin --target x86_64-pc-minix --release
    python tools/build-dynlibc.py x86

# What the loader's *own* gate needs on top of `dynlib-x86`: `tools/build-dynlink.py`
# builds `libdyn*.so` and `dynhello` (resolution, the region budget), `libdlopen.so`,
# `libtls1.so` and `dynopen` (the `dlopen` steps), whose strings live only in the objects
# and which no image ships. `test-dynlink-x86` injects them into a standard image
# (`DYNLINK_BINS`) and boots it.
dynlink-x86: dynlib-x86
    python tools/build-dynlink.py x86

# Build a C program of your own against the shared C library. `just cdyn tools/myprog.c`
# writes `target/dync/<arch>/<stem>`, and the program runs on a booted system with nothing
# else added: every image already carries `/lib/libc.so` and the loader the program asks
# for. `tools/cdyn.py` then prints the two commands that boot it — the second sets
# `MINIXFS_EXTRA`, and on Windows it is prefixed with `MSYS2_ENV_CONV_EXCL` so the `/bin/...`
# value survives MSYS's path conversion (a value that does not is refused by
# `crates/kernel/build.rs`).
#
# `tools/cdyn.py` is the same link `tools/build-dynlibc.py` gives the shipped
# `/bin/dynclib`, so a program of your own gets the flags that program is tested with, and
# `just test-cdyn-x86` is the gate on this recipe.
cdyn src arch="x86":
    python tools/cdyn.py {{arch}} {{src}}

# Build the C++ runtime (libc++ + libc++abi) for one minix target and merge them
# into target/cxx/<arch>/minix-runtime/libstdc++.a — the archive Mesa's shared
# objects link (tools/cc-dso-minix.py). x86 was the first arch; the others build
# the same way and only differ in the red-zone/march flags (tools/ccarch.py).
#
# Freestanding CMake cross builds (host clang targeting <machine>-unknown-none)
# against the Minix C headers (tools/c-include), driven by
# tools/build-libcxx.py, which owns the quirks this recipe used to spell out:
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
    python tools/build-libcxx.py {{arch}}

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
#
# Clippy runs with `--all-targets` so the lint set matches CI's `host-tests`
# job, which lints tests, examples and bins as well as the libraries.
#
# The physmap rule is the same kind of gate (`.notes/physmap.md` P6): a physical address is a
# number, not a pointer, and check-physmap.py rejects one cast to a pointer unless the line
# converts it (`phys_to_virt`/`frame_ptr`/`table_ptr`) or says `physmap-ok: <reason>`.
check:
    cargo clippy --all-targets -- -D warnings
    python tools/check-c-headers.py
    python tools/check-physmap.py
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
