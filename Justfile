# Build the 64-bit kernel (release) and run in QEMU.
#
# QEMU's built-in multiboot loader refuses ELF64 kernels, so we use a small
# ELF32 trampoline (built by crates/kernel-boot/build.rs) that transitions to
# 64-bit long mode and jumps to the kernel loaded at 0x200000 via -device loader.

set shell := ["cmd.exe", "/c"]

KERNEL     := "target\\x86_64-pc-minix\\release\\kernel-boot"
KERNEL_BIN := "target\\kernel.bin"
TRAMP_ELF  := "target\\trampoline.elf"
TARGET     := "x86_64-pc-minix.json"
OBJCOPY    := "rust-objcopy"
RUSTUP     := "rustup"
QEMU       := "qemu-system-x86_64"
CLANG      := "clang"
RUST_LD    := "rust-lld"
RUST_NM    := "rust-nm"

# Build the 64-bit kernel binary, then build the trampoline with correct kmain address.
build:
    @rustc tools\mkboot.rs --edition 2024 -o target\mkboot.exe 2>nul
    target\mkboot.exe

# Build + run in QEMU (uses default SeaBIOS)
run: build
    {{QEMU}} -nographic -m 256M -no-reboot -kernel {{TRAMP_ELF}} -device loader,file={{KERNEL_BIN}},addr=0x200000

# Build and run QEMU integration tests.
# Boots the kernel without userland, runs assertions, exits via isa-debug-exit.
test-qemu:
    @rustc tools\mkboot.rs --edition 2024 -o target\mkboot-test.exe 2>nul
    @target\mkboot-test.exe embed_initramfs,integration-tests
    @{{QEMU}} -nographic -m 256M -no-reboot -device isa-debug-exit -kernel target\trampoline.elf -device loader,file=target\kernel.bin,addr=0x200000

# Build a bootable disk image (minix.img) and run via SeaBIOS
image: build
    @rustc tools/mkimg.rs --out-dir target 2>nul || rustc tools/mkimg.rs -o target\\mkimg.exe
    target\\mkimg.exe

# Run the disk image directly
run-img: image
    {{QEMU}} -nographic -serial mon:stdio -m 256M -no-reboot -drive format=raw,file=target\\minix.img

QEMU_RV := "qemu-system-riscv64"
RV_TARGET := "riscv64gc-unknown-none-elf"

# Build the initramfs with RISC-V userland binaries.
build-initramfs-riscv64:
    @rustc tools\mkinitramfs.rs --edition 2024 -o target\mkinitramfs.exe 2>nul
    target\mkinitramfs.exe riscv64
    @rustc tools\mkminixfs.rs --edition 2021 -o target\mkminixfs.exe 2>nul
    -target\mkminixfs.exe riscv64

# Build the RISC-V64 kernel binary (requires nightly for -Zbuild-std).
# Linker script is set in .cargo/config.toml.
build-riscv64: build-initramfs-riscv64
    rustup run nightly cargo build -p kernel-boot --bin kernel-boot-riscv64 --target {{RV_TARGET}} --features embed_initramfs,embed_minixfs,riscv64 -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release

# Run the RISC-V64 kernel in QEMU (uses OpenSBI built-in).
run-riscv64: build-riscv64
    {{QEMU_RV}} -machine virt -m 256M -nographic -kernel target/riscv64gc-unknown-none-elf/release/kernel-boot-riscv64
