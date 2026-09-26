//! `ld.so` — the dynamic loader for the minix port.
//!
//! VFS installs this binary as a program's `PT_INTERP` interpreter, the kernel
//! enters it with the main program's ELF header page in a register, and [`rtld`]
//! maps each `DT_NEEDED` shared object at a base it allocates, resolves
//! `GLOB_DAT`/`JUMP_SLOT` from the objects' symbol tables, applies every object's
//! `RELATIVE` fixups against its base, places the thread-local storage the objects
//! brought, and hands back the main program's entry point.
//!
//! [`elf`], [`layout`], [`reloc`] and [`search`] are host-buildable and carry the
//! unit tests (`cargo test -p ldso`), so the parsing, the placement, the relocation
//! rules and the name resolution are checked without a guest. [`rtld`] and the
//! `ldso` binary are minix-only.
#![cfg_attr(not(test), no_std)]
// The loader's errors carry a symbol name so a failure can say *which* symbol,
// and there is no allocator here to box one with. They travel once, on the cold
// path — a load either succeeds wholly or dies — so the size is not a cost.
#![allow(clippy::result_large_err)]

pub mod elf;
pub mod layout;
pub mod reloc;
pub mod search;

#[cfg(target_os = "minix")]
pub mod rtld;
