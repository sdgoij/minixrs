# Build and run the MINIX/Rust port in QEMU.
#
# Targets: x86 (default), riscv64, aarch64.
#
# Requires:
#   - `just bootstrap` first (builds the rust fork stage1 compiler, which
#     provides the in-tree minix targets + their std sysroot)
#   - QEMU, rust-nm, rust-lld, rust-objcopy, clang
#
# The recipes orchestrate plain `cargo` invocations; all image assembly
# (initramfs CPIO + MinixFS) lives in `crates/kernel/build.rs`. x86
# post-link work (trampoline + kernel.bin) lives in `tools/mkboot.rs`.

# Path to the rust fork's stage1 rustc (built by `just bootstrap`); used as
# the RUSTC for userland/server builds so the in-tree minix targets and
# their std sysroot are used (no `-Zbuild-std`, no JSON specs).
stage1-rustc := `ls rust/build/*/stage1/bin/rustc.exe rust/build/*/stage1/bin/rustc 2>/dev/null | head -1`

# Fetches the rust fork submodule first when it's missing
# (`git submodule update --init rust` is a no-op when it's already present
# and at the pinned commit), regenerates `rust/config.toml` via
# `tools/rust-config.py` for the requested arch, then runs x.py and builds
# the `/bin/hello` std smoke-test binary with `tools/build-std-hello.py`.
# Note: per-arch runs rebuild only that arch's std — x.py prunes the other
# arches from the stage1 sysroot — so `all` (the default) is the complete
# setup. Incremental afterwards; the first run downloads the stage0
# toolchain and CI LLVM (needs network).
# Build the stage1 compiler + std + /bin/hello for a minix target (all by default).
bootstrap target="all":
    git submodule update --init rust
    python tools/rust-config.py {{target}}
    cd rust && python x.py build library/std
    # x.py rebuilt the stage1 rustc, but cargo fingerprints the compiler by
    # version string — an incremental rebuild keeps the same string, so the
    # old rlib cache stays "fresh" and the next userland build fails with
    # E0463 ("can't find crate for ...") when rustc cannot read the stale
    # metadata. Drop the cargo cache; the smoke-test binaries under target/
    # are rebuilt below.
    cargo clean
    python tools/build-std-hello.py {{target}}
    # The C smoke-test binaries (helloc/ctest) also live under target/ and
    # are wiped by the clean; rebuild them for x86 (build-c-hello.py is
    # x86-only — riscv64/aarch64 C binaries are not yet supported).
    if [ "{{target}}" = x86 -o "{{target}}" = all ]; then python tools/build-c-hello.py; fi
    @echo "stage1 compiler + /bin/hello ready. Re-assemble images: target/mkboot embed_initramfs,embed_minixfs && target/mkfs <arch>"

# Userland + server binaries for a target, built into the shared cargo
# target dir (fast incremental; required before the kernel build, whose
# build.rs assembles the images from them).
userland-x86:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target x86_64-pc-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target x86_64-pc-minix --release

userland-riscv64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target riscv64gc-unknown-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target riscv64gc-unknown-minix --release

userland-aarch64:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p userland --bins --target aarch64-unknown-minix --release
    RUSTC="{{stage1-rustc}}" RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" cargo build -p servers --bins --target aarch64-unknown-minix --release

# ---------- build ----------

build target="x86":
    @just build-{{target}}

build-x86: userland-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs

build-x86-test: userland-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,integration-tests

build-x86-boot: userland-x86
    rm -f target/mkboot target/mkboot.exe
    "{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,boot-test

build-riscv64: userland-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64 --release

build-aarch64: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64 --release

# ---------- run ----------

run target="x86":
    @just run-{{target}}

run-x86: build-x86 mkfs-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0 -netdev user,id=net0 -device virtio-net-pci,disable-legacy=on,netdev=net0

run-riscv64: build-riscv64 mkfs-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64

run-aarch64: build-aarch64 mkfs-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -netdev user,id=net0 -device virtio-net-device,netdev=net0 -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- debug (QEMU gdb stub on :1234) ----------

debug target="x86":
    @just debug-{{target}}

debug-x86: build-x86 mkfs-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -s -S -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0

debug-aarch64: build-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -display none -serial stdio -no-reboot -s -S -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- tests ----------

# RISC-V/AArch64 test builds (fork stage1 compiler, in-tree targets). The
# integration-tests feature runs kernel::tests::run_all() in QEMU before any
# userspace starts; boot-test runs the multi-server boot suite after VFS
# mount_root.

build-riscv64-test: userland-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,integration-tests --release

build-aarch64-test: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,integration-tests --release

build-riscv64-boot: userland-riscv64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-minix --features embed_initramfs,embed_minixfs,riscv64,boot-test --release

build-aarch64-boot: userland-aarch64
    RUSTC="{{stage1-rustc}}" cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix --features embed_initramfs,embed_minixfs,aarch64,boot-test --release

test-qemu target="x86":
    @just test-qemu-{{target}}

test-qemu-x86: build-x86-test mkfs-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -device isa-debug-exit -monitor none -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

# RISC-V exits via SBI SRST (no exit-code device in this QEMU build), so
# pass/fail is determined from the serial log.
test-qemu-riscv64: build-riscv64-test
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

# AArch64 exits via PSCI SYSTEM_OFF (always exit code 0 — no exit-code
# device and no semihosting on this QEMU build), so pass/fail is determined
# from the serial log like RISC-V.
test-qemu-aarch64: build-aarch64-test
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test-boot target="x86":
    @just test-boot-{{target}}

test-boot-x86: build-x86-boot mkfs-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -device isa-debug-exit -monitor none -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-pci,disable-legacy=on,drive=disk0; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test-boot-riscv64: build-riscv64-boot mkfs-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test-boot-aarch64: build-aarch64-boot mkfs-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -global virtio-mmio.force-legacy=off -drive if=none,id=disk0,file=target/disk.img,format=raw,cache=writethrough -device virtio-blk-device,drive=disk0 -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test target="x86":
    @just test-{{target}}

test-riscv64: build-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-minix/release/kernel-boot-riscv64

test-kernel target="x86":
    @just test-qemu {{target}}

# Write the root filesystem blob to target/disk.img for the virtio-blk drive.
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

# Rebuild the C smoke-test binary (/bin/helloc) from tools/hello.c +
# tools/crt0-x86_64.S (clang freestanding + minix-libc, linked with the fork
# rustc), then re-embed it in the initramfs and disk image. Requires
# `just build-x86` once so target/mkboot exists.
build-c-hello:
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    python tools/build-c-hello.py
    @test -x target/mkboot || (echo 'error: target/mkboot missing — run `just build-x86` once' >&2 && exit 1)
    target/mkboot embed_initramfs,embed_minixfs
    rm -f target/mkfs target/mkfs.exe
    "{{stage1-rustc}}" tools/mkfs.rs --edition 2021 -o target/mkfs
    target/mkfs x86_64

# ---------- check ----------

# Host clippy + riscv64 compilation check (fork stage1 compiler).
check:
    cargo clippy -- -D warnings
    @test -n "{{stage1-rustc}}" || (echo 'error: stage1 rustc not found — run `just bootstrap` first' >&2 && exit 1)
    RUSTC="{{stage1-rustc}}" cargo check -p kernel-boot --bin kernel-boot-riscv64 --features riscv64 --target riscv64gc-unknown-minix --release

# Remove generated assets that must be rebuilt from scratch.
clean:
    rm -rf target/nested target/images
    rm -f target/initramfs.cpio target/minixfs.img target/trampoline.elf target/trampoline_.o target/kernel.bin target/mkboot target/mkboot.exe
