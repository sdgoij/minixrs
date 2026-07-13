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

build target="x86":
    @build {{target}}

run target="x86": build
    @run {{target}}

debug target="x86": build
    @debug {{target}}

test-qemu target="x86":
    @test {{target}}
