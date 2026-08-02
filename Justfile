# Build and run the MINIX/Rust port in QEMU.
#
# Targets: x86 (default), riscv64, aarch64.
#
# Requires:
#   - nightly Rust via rustup (`-Zbuild-std`)
#   - QEMU, rust-nm, rust-lld, rust-objcopy, clang
#
# The recipes orchestrate plain `cargo` invocations; all image assembly
# (initramfs CPIO + MinixFS) lives in `crates/kernel/build.rs`. x86
# post-link work (trampoline + kernel.bin) lives in `tools/mkboot.rs`.

# Userland + server binaries for a target, built into the shared cargo
# target dir (fast incremental; required before the kernel build, whose
# build.rs assembles the images from them).
userland-x86:
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p userland --bins --target x86_64-pc-minix.json -Zunstable-options -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p servers --bins --target x86_64-pc-minix.json -Zunstable-options -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

userland-riscv64:
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p userland --bins --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p servers --bins --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

userland-aarch64:
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p userland --bins --target aarch64-unknown-minix.json -Zunstable-options -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release
    RUSTFLAGS="-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr" rustup run nightly cargo build -p servers --bins --target aarch64-unknown-minix.json -Zunstable-options -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

# ---------- build ----------

build target="x86":
    @just build-{{target}}

build-x86: userland-x86
    rustc tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs

build-x86-test: userland-x86
    rustc tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,integration-tests

build-x86-boot: userland-x86
    rustc tools/mkboot.rs --edition 2024 -o target/mkboot
    target/mkboot embed_initramfs,embed_minixfs,boot-test

build-riscv64: userland-riscv64
    rustup run nightly cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-none-elf --features embed_initramfs,embed_minixfs,riscv64 -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

build-aarch64: userland-aarch64
    rustup run nightly cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix.json --features embed_initramfs,embed_minixfs,aarch64 -Zunstable-options -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

# ---------- run ----------

run target="x86":
    @just run-{{target}}

run-x86: build-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000

run-riscv64: build-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-none-elf/release/kernel-boot-riscv64

run-aarch64: build-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -nographic -no-reboot -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- debug (QEMU gdb stub on :1234) ----------

debug target="x86":
    @just debug-{{target}}

debug-x86: build-x86
    qemu-system-x86_64 -nographic -m 256M -no-reboot -s -S -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000

debug-aarch64: build-aarch64
    qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -display none -serial stdio -no-reboot -s -S -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# ---------- tests ----------

test-qemu target="x86":
    @just test-qemu-{{target}}

test-qemu-x86: build-x86-test
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -device isa-debug-exit -monitor none; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test-qemu-riscv64: build-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-none-elf/release/kernel-boot-riscv64

test-boot target="x86":
    @just test-boot-{{target}}

test-boot-x86: build-x86-boot
    qemu-system-x86_64 -nographic -m 256M -no-reboot -kernel target/trampoline.elf -device loader,file=target/kernel.bin,addr=0x200000 -device isa-debug-exit -monitor none; code=$?; if [ "$code" -eq 1 ]; then exit 0; else exit "$code"; fi

test target="x86":
    @just test-{{target}}

test-riscv64: build-riscv64
    qemu-system-riscv64 -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-none-elf/release/kernel-boot-riscv64

test-kernel target="x86":
    @just test-qemu {{target}}

# ---------- check ----------

# Host clippy + riscv64 compilation check.
check:
    cargo clippy -- -D warnings
    cargo check -p kernel-boot --lib --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
    cargo check -p kernel-boot --bin kernel-boot-riscv64 --features riscv64 --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Remove generated assets that must be rebuilt from scratch.
clean:
    rm -rf target/nested target/images
    rm -f target/initramfs.cpio target/minixfs.img target/trampoline.elf target/trampoline_.o target/kernel.bin target/mkboot target/mkboot.exe
