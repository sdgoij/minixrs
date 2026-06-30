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
# Exit code 1 = all passed, exit code >1 = (failures << 1) | 1.
test-qemu: build
    @set RUSTFLAGS=-C link-arg=-Ttools/minix-raw.ld && {{RUSTUP}} run nightly cargo build -p kernel-boot --target {{TARGET}} -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --features embed_initramfs,integration-tests --release
    @{{OBJCOPY}} -O binary target\x86_64-pc-minix\release\kernel-boot target\test-kernel.bin
    @{{QEMU}} -nographic -m 256M -no-reboot -device isa-debug-exit -kernel {{TRAMP_ELF}} -device loader,file=target\test-kernel.bin,addr=0x200000 & if errorlevel 1 if not errorlevel 2 (echo QEMU_EXIT=1: all tests passed) else (echo QEMU_EXIT=%ERRORLEVEL%: some tests failed & exit /b 1)

# Build a bootable disk image (minix.img) and run via SeaBIOS
image: build
    @rustc tools/mkimg.rs --out-dir target 2>nul || rustc tools/mkimg.rs -o target\\mkimg.exe
    target\\mkimg.exe

# Run the disk image directly
run-img: image
    {{QEMU}} -nographic -serial mon:stdio -m 256M -no-reboot -drive format=raw,file=target\\minix.img
