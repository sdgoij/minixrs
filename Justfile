# Build the 64-bit kernel and run in QEMU.
# Also supports RISC-V 64 (riscv64gc-unknown-none-elf).
#
# Prerequisite: compile jsh once:
#   rustc tools/jsh.rs -o target/jsh
#
# On Unix, `just prepare` handles this automatically.
# On Windows, run the command above manually.

[unix]
prepare:
    #!/usr/bin/env sh
    rustc tools/jsh.rs -o target/jsh

set shell := ["target/jsh", "-c"]

# Check code quality across all targets.
# Host clippy (fast), riscv64 check (compilation verification).
check:
    cargo clippy -- -D warnings
    cargo check -p kernel-boot --lib --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
    cargo check -p kernel-boot --bin kernel-boot-riscv64 --features riscv64 --target riscv64gc-unknown-none-elf -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

# Remove generated assets that jsh doesn't rebuild if they exist.
# Run this if you suspect stale generated data (e.g. after arch switch).
clean:
    rm -f target/initramfs_data.rs target/minixfs_data.rs target/jsh.exe target/jsh_new.exe target/mkinitramfs.exe target/mkminixfs.exe target/mkboot.exe

build target="x86":
    @build {{target}}

run target="x86": build
    @run {{target}}

debug target="x86": build
    @debug {{target}}

test-qemu target="x86":
    @test-qemu {{target}}

test-boot target="x86":
    @test-boot {{target}}

test-kernel target="x86":
    @test-qemu {{target}}
