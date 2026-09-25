//! `ld.so` — the dynamic loader for the minix port.
//!
//! Phase 0 is classic non-PIE linking: VFS installs this binary as a program's
//! `PT_INTERP` interpreter, the kernel enters it with the main program's ELF
//! header page in a register, and [`rtld`] maps each `DT_NEEDED` shared object,
//! resolves `GLOB_DAT`/`JUMP_SLOT` from the objects' symbol tables, applies
//! `RELATIVE` fixups, and hands back the main program's entry point.
//!
//! [`elf`] and [`reloc`] are host-buildable and carry the unit tests
//! (`cargo test -p ldso`), so the parsing and relocation rules are checked
//! without a guest. [`rtld`] and the `ldso` binary are minix-only.
#![cfg_attr(not(test), no_std)]

pub mod elf;
pub mod reloc;

#[cfg(target_os = "minix")]
pub mod rtld;
