//! The relocation rules the loader applies.
//!
//! Kept apart from the loader's memory access so the value a relocation writes is
//! unit-tested directly: [`reloc_action`] is the whole decision, and the loader
//! is only responsible for storing the returned value at `r_offset`.

use crate::elf::{
    R_X86_64_64, R_X86_64_COPY, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT, R_X86_64_NONE,
    R_X86_64_RELATIVE,
};

/// What to do with one relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to write (a no-op relocation).
    Skip,
    /// Store this at `r_offset`.
    Write(u64),
    /// A relocation type this loader does not implement; the load must fail
    /// rather than continue with a half-relocated image.
    Unsupported,
}

/// Resolve one relocation. `base` is the loaded image's base address (the value
/// a `RELATIVE` fixup adds to); `sym_value` is the resolved symbol's address,
/// ignored by `RELATIVE`.
pub fn reloc_action(typ: u32, base: u64, sym_value: u64, addend: i64) -> Action {
    match typ {
        R_X86_64_NONE => Action::Skip,
        R_X86_64_RELATIVE => Action::Write(base.wrapping_add(addend as u64)),
        R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
            Action::Write(sym_value.wrapping_add(addend as u64))
        }
        R_X86_64_COPY => Action::Unsupported,
        _ => Action::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x200_0000;

    #[test]
    fn relative_adds_the_base_to_the_addend() {
        assert_eq!(
            reloc_action(R_X86_64_RELATIVE, BASE, 0, 0x1234),
            Action::Write(BASE + 0x1234)
        );
        // A negative addend wraps, as the ABI specifies.
        assert_eq!(
            reloc_action(R_X86_64_RELATIVE, BASE, 0, -8),
            Action::Write(BASE - 8)
        );
    }

    #[test]
    fn symbol_bindings_use_the_symbol_value() {
        for typ in [R_X86_64_64, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT] {
            assert_eq!(
                reloc_action(typ, BASE, 0x201_0000, 0),
                Action::Write(0x201_0000),
                "type {typ}"
            );
        }
    }

    #[test]
    fn relocation_model_is_deterministic_with_the_linker() {
        // RELATIVE ignores the symbol value (there is none).
        assert_eq!(
            reloc_action(R_X86_64_RELATIVE, BASE, 0xdead_0000, 0),
            Action::Write(BASE)
        );
    }

    #[test]
    fn none_skips_and_unsupported_is_refused() {
        assert_eq!(reloc_action(R_X86_64_NONE, BASE, 0, 0), Action::Skip);
        assert_eq!(reloc_action(R_X86_64_COPY, BASE, 0, 0), Action::Unsupported);
        assert_eq!(reloc_action(0xdead_beef, BASE, 0, 0), Action::Unsupported);
    }
}
