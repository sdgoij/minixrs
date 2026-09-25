//! The relocation rules the loader applies.
//!
//! [`reloc_action`] is the whole per-relocation decision and [`apply_table`] is
//! the walk that drives it. Both are kept apart from real memory so they are
//! unit-tested directly (`cargo test -p ldso`); what an image has to provide is
//! the small [`RelocImage`] surface — its relocation table, a symbol's name, and
//! a store — and the loader and the host tests each implement it over their own
//! idea of "the image".

use crate::elf::{
    R_X86_64_64, R_X86_64_COPY, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT, R_X86_64_NONE,
    R_X86_64_RELATIVE, RELA_SIZE, Rela,
};

/// What to do with one relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to write (a no-op relocation).
    Skip,
    /// Store this at `r_offset`.
    Write(u64),
    /// A byte copy (`R_X86_64_COPY`): the walk resolves the symbol with
    /// [`Scope::ExcludeSelf`] and moves the definition's bytes to `r_offset`.
    Copy,
    /// A relocation type this loader does not implement; the load must fail
    /// rather than continue with a half-relocated image.
    Unsupported,
}

/// Resolve one relocation. `base` is the loaded image's base address (the value
/// a `RELATIVE` fixup adds to); `sym_value` is the resolved symbol's address,
/// ignored by `RELATIVE` (and by `COPY`, whose bytes the walk moves itself).
pub fn reloc_action(typ: u32, base: u64, sym_value: u64, addend: i64) -> Action {
    match typ {
        R_X86_64_NONE => Action::Skip,
        R_X86_64_RELATIVE => Action::Write(base.wrapping_add(addend as u64)),
        R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
            Action::Write(sym_value.wrapping_add(addend as u64))
        }
        // A non-PIE executable's reference to a variable defined in a shared object:
        // the linker reserved space for the variable in the executable, and the
        // loader has to move the object's initial value into it.
        R_X86_64_COPY => Action::Copy,
        _ => Action::Unsupported,
    }
}

/// Whether a relocation type's *value* is a symbol's address. `COPY` is not one
/// of these: its bytes are moved by the walk, and its definition is looked up
/// with a narrower scope than a value's.
pub fn uses_symbol(typ: u32) -> bool {
    matches!(typ, R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT)
}

/// Which loaded objects a symbol lookup may consider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every loaded object.
    All,
    /// Every object but the one being relocated. A `COPY` needs this: this image's
    /// own symbol for the name *is* the destination, so a copy that resolved it
    /// would read what it is about to write.
    ExcludeSelf,
}

/// A symbol's definition, as its defining object states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    /// Its runtime address in the object that defines it.
    pub addr: u64,
    /// `st_size` — how many bytes a `COPY` moves.
    pub size: u64,
}

/// Longest symbol name a walk copies. A name that does not fit fails the load
/// rather than being truncated: a truncated name would bind to a different
/// symbol, or to none, and either is worse than refusing.
pub const SYMNAME_MAX: usize = 128;

/// A symbol name copied out of an image's string table.
///
/// The buffer is private and only `as_bytes` reads it, so equality is over the
/// name and not over whatever a longer name left behind it in the buffer.
#[derive(Debug, Clone, Copy)]
pub struct SymName {
    buf: [u8; SYMNAME_MAX],
    len: usize,
}

impl SymName {
    pub const fn new() -> Self {
        Self {
            buf: [0; SYMNAME_MAX],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Copy `name` in. `false` when it does not fit.
    pub fn set(&mut self, name: &[u8]) -> bool {
        if name.len() > SYMNAME_MAX {
            return false;
        }
        self.buf[..name.len()].copy_from_slice(name);
        self.buf[name.len()..].fill(0);
        self.len = name.len();
        true
    }
}

impl PartialEq for SymName {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for SymName {}

impl Default for SymName {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a symbol's name could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymError {
    /// The image has no name for that index (index 0, or past its table).
    Missing,
    /// The name is longer than [`SYMNAME_MAX`].
    TooLong,
}

/// Why a relocation walk failed. Every variant is a load failure: an image whose
/// relocations cannot all be applied must not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocError {
    /// A `RELA` table size that is not a whole number of entries.
    BadSize(usize),
    /// The table's `index`th entry is not inside the image.
    BadTable(u64, usize),
    /// A relocation type this loader does not implement.
    Unsupported(u32),
    /// The relocation's symbol has no usable name.
    BadSymbol(u32, SymError),
    /// The relocation's symbol is not defined by any loaded object.
    Unresolved(SymName),
    /// The relocation's target is outside the image it belongs to.
    OutOfRange(u64),
}

/// What a relocation walk needs from a loaded image.
///
/// Table and symbol reads take **link-time** virtual addresses — the values an
/// image's dynamic array holds — and each implementation maps them into its own
/// storage. A store or copy takes a **runtime** address, which is
/// `bias() + r_offset`.
pub trait RelocImage {
    /// The `index`th entry of the `RELA` table at link-time VA `table_va`.
    fn rela(&self, table_va: u64, index: usize) -> Option<Rela>;
    /// The name of dynamic symbol `idx`.
    fn sym_name(&self, idx: u32, out: &mut SymName) -> Result<(), SymError>;
    /// The base the image was loaded at: a link-time address `v` reads at
    /// `bias() + v` in this image.
    fn bias(&self) -> u64;
    /// Whether symbol `idx` is weak (`STB_WEAK`). A weak reference with no
    /// definition resolves to 0 rather than failing the load, as the ABI says.
    fn sym_is_weak(&self, idx: u32) -> bool;
    /// Store `value` at runtime VA `va`. `false` when `va` is not inside this
    /// image — a relocation may only write to its own object.
    fn store(&mut self, va: u64, value: u64) -> bool;
    /// Copy `len` bytes from runtime VA `src` to runtime VA `dst` (a `COPY`
    /// relocation). `false` when `dst .. dst + len` leaves this image.
    fn copy_range(&mut self, dst: u64, src: u64, len: u64) -> bool;
}

/// Apply every relocation in the `RELA` table at link-time VA `table_va` spanning
/// `bytes`, resolving names through `resolve`.
pub fn apply_table<I, F>(
    img: &mut I,
    table_va: u64,
    bytes: usize,
    resolve: &mut F,
) -> Result<(), RelocError>
where
    I: RelocImage,
    F: FnMut(&[u8], Scope) -> Option<Def>,
{
    if !bytes.is_multiple_of(RELA_SIZE) {
        return Err(RelocError::BadSize(bytes));
    }
    let base = img.bias();
    let mut name = SymName::new();
    for i in 0..bytes / RELA_SIZE {
        let r = img
            .rela(table_va, i)
            .ok_or(RelocError::BadTable(table_va, i))?;
        let typ = r.typ();
        let sym_value = if uses_symbol(typ) {
            img.sym_name(r.sym(), &mut name)
                .map_err(|e| RelocError::BadSymbol(r.sym(), e))?;
            match resolve(name.as_bytes(), Scope::All) {
                Some(def) => def.addr,
                // A weak reference with no definition is 0; a strong one is a load
                // failure, which is what eager binding buys (D6).
                None if img.sym_is_weak(r.sym()) => 0,
                None => return Err(RelocError::Unresolved(name)),
            }
        } else {
            0
        };
        match reloc_action(typ, base, sym_value, r.r_addend) {
            Action::Skip => {}
            Action::Write(value) => {
                let at = base.wrapping_add(r.r_offset);
                if !img.store(at, value) {
                    return Err(RelocError::OutOfRange(at));
                }
            }
            Action::Copy => {
                img.sym_name(r.sym(), &mut name)
                    .map_err(|e| RelocError::BadSymbol(r.sym(), e))?;
                let def = resolve(name.as_bytes(), Scope::ExcludeSelf)
                    .ok_or(RelocError::Unresolved(name))?;
                let at = base.wrapping_add(r.r_offset);
                if !img.copy_range(at, def.addr, def.size) {
                    return Err(RelocError::OutOfRange(at));
                }
            }
            Action::Unsupported => return Err(RelocError::Unsupported(typ)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{
        EHDR_SIZE, ELF_MAGIC, ELFCLASS64, ELFDATA2LSB, EM_X86_64, ET_DYN, Elf, PF_R, PF_W,
        PHDR_SIZE, PT_LOAD, SYM_SIZE,
    };

    /// The base the synthetic image is "loaded" at. Its own link-time addresses
    /// start at 0, so a runtime address is `BASE + link-time address`.
    const BASE: u64 = 0x200_0000;
    const PHOFF: usize = EHDR_SIZE;
    const STRTAB_VA: u64 = 0x100;
    const SYMTAB_VA: u64 = 0x128;
    const RELA_VA: u64 = 0x1c0;
    const PLT_VA: u64 = 0x220;
    const FILE_LEN: usize = 0x400;
    const NSYM: usize = 6;

    // Where the relocations write. `SLOT_NONE` is a no-op's target: it must come
    // out untouched, which is how "applied to the right offset" is checked for
    // the type that writes nothing.
    const SLOT_RELATIVE: u64 = 0x380;
    const SLOT_ABS64: u64 = 0x388;
    const SLOT_JUMP: u64 = 0x390;
    const SLOT_GLOB: u64 = 0x398;
    const SLOT_NONE: u64 = 0x3a0;
    const SLOT_COPY: u64 = 0x3a8;
    const SLOT_WEAK: u64 = 0x3b0;
    /// The COPY's source — the "other object's" own bytes, staged by the test. A
    /// copy is memory to memory, so where they live does not matter here: what the
    /// walk must get right is which objects it may resolve the name in.
    const SOURCE: u64 = 0x3c0;

    const RESOLVED_MESSAGE: u64 = 0x200_1234;
    const RESOLVED_CROSS: u64 = 0x201_5678;
    const SENTINEL: u8 = 0xaa;

    fn wr16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn wr32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn wr64(b: &mut [u8], o: usize, v: u64) {
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn r_info(sym: u32, typ: u32) -> u64 {
        ((sym as u64) << 32) | typ as u64
    }

    /// A synthetic `ET_DYN` image with one `R|W` `PT_LOAD` covering the file, a
    /// symbol table (`dyn_message` defined; `cross`, `nowhere` and `copied`
    /// undefined, so they resolve to another object if at all, and `maybe`
    /// undefined and weak), and two relocation tables: `.rela.dyn` with `RELATIVE`,
    /// `R_X86_64_64`, `NONE` and `COPY`, `.rela.plt` with `JUMP_SLOT` and two
    /// `GLOB_DAT`s, one of them against the weak symbol.
    fn file_image() -> Vec<u8> {
        let mut b = vec![0u8; FILE_LEN];
        b[0..4].copy_from_slice(&ELF_MAGIC);
        b[4] = ELFCLASS64;
        b[5] = ELFDATA2LSB;
        b[6] = 1;
        wr16(&mut b, 16, ET_DYN);
        wr16(&mut b, 18, EM_X86_64);
        wr32(&mut b, 20, 1);
        wr64(&mut b, 24, 0x40);
        wr64(&mut b, 32, PHOFF as u64);
        wr16(&mut b, 52, EHDR_SIZE as u16);
        wr16(&mut b, 54, PHDR_SIZE as u16);
        wr16(&mut b, 56, 1);
        wr32(&mut b, PHOFF, PT_LOAD);
        wr32(&mut b, PHOFF + 4, PF_R | PF_W);
        wr64(&mut b, PHOFF + 32, FILE_LEN as u64);
        wr64(&mut b, PHOFF + 40, FILE_LEN as u64);
        wr64(&mut b, PHOFF + 48, 0x1000);

        let strtab = STRTAB_VA as usize;
        b[strtab..strtab + 40].copy_from_slice(b"\0dyn_message\0cross\0nowhere\0copied\0maybe\0");

        let sym = SYMTAB_VA as usize;
        wr32(&mut b, sym + SYM_SIZE, 1); // st_name -> "dyn_message"
        b[sym + SYM_SIZE + 4] = 0x11; // GLOBAL | OBJECT
        wr16(&mut b, sym + SYM_SIZE + 6, 1); // defined
        wr64(&mut b, sym + SYM_SIZE + 8, 0x40);
        wr64(&mut b, sym + SYM_SIZE + 16, 8);
        wr32(&mut b, sym + 2 * SYM_SIZE, 13); // "cross", undefined
        b[sym + 2 * SYM_SIZE + 4] = 0x11;
        wr32(&mut b, sym + 3 * SYM_SIZE, 19); // "nowhere", undefined
        b[sym + 3 * SYM_SIZE + 4] = 0x11;
        wr32(&mut b, sym + 4 * SYM_SIZE, 27); // "copied", undefined: a COPY's name
        b[sym + 4 * SYM_SIZE + 4] = 0x11;
        wr32(&mut b, sym + 5 * SYM_SIZE, 34); // "maybe", undefined and WEAK
        b[sym + 5 * SYM_SIZE + 4] = 0x21; // WEAK | OBJECT

        let rela = RELA_VA as usize;
        wr64(&mut b, rela, SLOT_RELATIVE);
        wr64(&mut b, rela + 8, r_info(0, R_X86_64_RELATIVE));
        wr64(&mut b, rela + 16, 0x40);
        wr64(&mut b, rela + RELA_SIZE, SLOT_ABS64);
        wr64(&mut b, rela + RELA_SIZE + 8, r_info(1, R_X86_64_64));
        wr64(&mut b, rela + RELA_SIZE + 16, 0x10);
        wr64(&mut b, rela + 2 * RELA_SIZE, SLOT_NONE);
        wr64(&mut b, rela + 2 * RELA_SIZE + 8, r_info(0, R_X86_64_NONE));
        wr64(&mut b, rela + 3 * RELA_SIZE, SLOT_COPY);
        wr64(&mut b, rela + 3 * RELA_SIZE + 8, r_info(4, R_X86_64_COPY));

        let plt = PLT_VA as usize;
        wr64(&mut b, plt, SLOT_JUMP);
        wr64(&mut b, plt + 8, r_info(2, R_X86_64_JUMP_SLOT));
        wr64(&mut b, plt + RELA_SIZE, SLOT_GLOB);
        wr64(&mut b, plt + RELA_SIZE + 8, r_info(1, R_X86_64_GLOB_DAT));
        wr64(&mut b, plt + 2 * RELA_SIZE, SLOT_WEAK);
        wr64(
            &mut b,
            plt + 2 * RELA_SIZE + 8,
            r_info(5, R_X86_64_GLOB_DAT),
        );
        b
    }

    /// The "loaded image": the file's bytes laid out at the image's own link-time
    /// addresses, so a runtime address indexes it at `va - BASE`. Kept separate
    /// from the file buffer the tables are parsed from, because a walk reads the
    /// tables and writes the memory — the loader's two views of one object.
    struct Synth<'f, 'm> {
        file: Elf<'f>,
        img: &'m mut [u8],
        base: u64,
    }

    impl RelocImage for Synth<'_, '_> {
        fn rela(&self, table_va: u64, index: usize) -> Option<Rela> {
            self.file.rela_at(table_va, index)
        }

        fn sym_name(&self, idx: u32, out: &mut SymName) -> Result<(), SymError> {
            if idx as usize >= NSYM {
                return Err(SymError::Missing);
            }
            let sym = self.file.sym_at(SYMTAB_VA, idx).ok_or(SymError::Missing)?;
            if sym.st_name == 0 {
                return Err(SymError::Missing);
            }
            let name = self
                .file
                .cstr(STRTAB_VA + sym.st_name as u64)
                .ok_or(SymError::Missing)?;
            if out.set(name) {
                Ok(())
            } else {
                Err(SymError::TooLong)
            }
        }

        fn bias(&self) -> u64 {
            self.base
        }

        fn sym_is_weak(&self, idx: u32) -> bool {
            self.file
                .sym_at(SYMTAB_VA, idx)
                .is_some_and(|s| s.bind() == crate::elf::Sym::STB_WEAK)
        }

        fn store(&mut self, va: u64, value: u64) -> bool {
            let Some(off) = va.checked_sub(self.base).map(|v| v as usize) else {
                return false;
            };
            if off + 8 > self.img.len() {
                return false;
            }
            self.img[off..off + 8].copy_from_slice(&value.to_le_bytes());
            true
        }

        fn copy_range(&mut self, dst: u64, src: u64, len: u64) -> bool {
            let len = len as usize;
            let (Some(d), Some(s)) = (
                dst.checked_sub(self.base).map(|v| v as usize),
                src.checked_sub(self.base).map(|v| v as usize),
            ) else {
                return false;
            };
            if d + len > self.img.len() || s + len > self.img.len() {
                return false;
            }
            self.img.copy_within(s..s + len, d);
            true
        }
    }

    fn resolve(name: &[u8], _scope: Scope) -> Option<Def> {
        match name {
            b"dyn_message" => Some(Def {
                addr: RESOLVED_MESSAGE,
                size: 0,
            }),
            b"cross" => Some(Def {
                addr: RESOLVED_CROSS,
                size: 0,
            }),
            // A COPY's definition: eight bytes the test staged at `SOURCE`.
            b"copied" => Some(Def {
                addr: BASE + SOURCE,
                size: 8,
            }),
            _ => None,
        }
    }

    /// Apply a table and report the scopes the resolver was called with, so a test
    /// can see *how* each type resolved and not only what it wrote.
    fn apply(
        file: &[u8],
        loaded: &mut [u8],
        table_va: u64,
        bytes: usize,
    ) -> (Result<(), RelocError>, Vec<Scope>) {
        let elf = Elf::new(file).expect("valid");
        let mut img = Synth {
            file: elf,
            img: loaded,
            base: BASE,
        };
        let mut scopes = Vec::new();
        let mut res = |name: &[u8], scope: Scope| {
            scopes.push(scope);
            resolve(name, scope)
        };
        let r = apply_table(&mut img, table_va, bytes, &mut res);
        (r, scopes)
    }

    fn slot(loaded: &[u8], off: u64) -> u64 {
        u64::from_le_bytes(loaded[off as usize..off as usize + 8].try_into().unwrap())
    }

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
        assert_eq!(reloc_action(R_X86_64_COPY, BASE, 0, 0), Action::Copy);
        assert_eq!(reloc_action(0xdead_beef, BASE, 0, 0), Action::Unsupported);
    }

    /// Every type this loader claims to handle is applied, at its own offset,
    /// exactly once — and the no-op's target is the control that says the walk
    /// wrote nothing else.
    #[test]
    fn every_reloc_type_lands_once_at_its_own_offset() {
        let file = file_image();
        let mut loaded = vec![SENTINEL; FILE_LEN];
        let (r, scopes) = apply(&file, &mut loaded, RELA_VA, 4 * RELA_SIZE);
        r.expect("dyn table");
        let (r, plt_scopes) = apply(&file, &mut loaded, PLT_VA, 3 * RELA_SIZE);
        r.expect("plt table");

        assert_eq!(
            slot(&loaded, SLOT_RELATIVE),
            BASE + 0x40,
            "RELATIVE is base + addend, not base + base + addend"
        );
        assert_eq!(
            slot(&loaded, SLOT_ABS64),
            RESOLVED_MESSAGE + 0x10,
            "R_X86_64_64 is the resolved symbol plus the addend"
        );
        assert_eq!(slot(&loaded, SLOT_JUMP), RESOLVED_CROSS, "JUMP_SLOT");
        assert_eq!(slot(&loaded, SLOT_GLOB), RESOLVED_MESSAGE, "GLOB_DAT");
        assert_eq!(
            slot(&loaded, SLOT_WEAK),
            0,
            "a weak undefined symbol is 0, not an error"
        );
        assert_eq!(
            slot(&loaded, SLOT_NONE),
            u64::from_le_bytes([SENTINEL; 8]),
            "NONE writes nothing"
        );

        // A value is resolved across every object; a COPY must not resolve to the
        // image it is writing into.
        assert_eq!(scopes, vec![Scope::All, Scope::ExcludeSelf]);
        assert_eq!(plt_scopes, vec![Scope::All, Scope::All, Scope::All]);

        let touched = [
            SLOT_RELATIVE,
            SLOT_ABS64,
            SLOT_JUMP,
            SLOT_GLOB,
            SLOT_NONE,
            SLOT_COPY,
            SLOT_WEAK,
            SOURCE,
        ];
        for (i, eight) in loaded.chunks_exact(8).enumerate() {
            let off = (i * 8) as u64;
            if touched.contains(&off) {
                continue;
            }
            assert!(
                eight.iter().all(|&b| b == SENTINEL),
                "the walk wrote at {off:#x}, which no relocation targets"
            );
        }
    }

    #[test]
    fn a_weak_undefined_symbol_resolves_to_zero_but_a_strong_one_fails() {
        let file = file_image();
        let mut loaded = vec![SENTINEL; FILE_LEN];
        let (r, _) = apply(&file, &mut loaded, PLT_VA, 3 * RELA_SIZE);
        r.expect("a weak reference is not a load failure");
        assert_eq!(slot(&loaded, SLOT_WEAK), 0);

        // The same relocation pointed at a strong undefined symbol instead.
        let mut file = file_image();
        wr64(
            &mut file,
            PLT_VA as usize + 2 * RELA_SIZE + 8,
            r_info(3, R_X86_64_GLOB_DAT),
        );
        let mut loaded = vec![SENTINEL; FILE_LEN];
        let mut name = SymName::new();
        assert!(name.set(b"nowhere"));
        assert_eq!(
            apply(&file, &mut loaded, PLT_VA, 3 * RELA_SIZE).0,
            Err(RelocError::Unresolved(name))
        );
    }

    #[test]
    fn a_copy_moves_the_definitions_bytes_and_resolves_outside_this_image() {
        let file = file_image();
        let mut loaded = vec![SENTINEL; FILE_LEN];
        let staged = [1u8, 2, 3, 4, 5, 6, 7, 8];
        loaded[SOURCE as usize..SOURCE as usize + 8].copy_from_slice(&staged);

        let (r, scopes) = apply(&file, &mut loaded, RELA_VA, 4 * RELA_SIZE);
        r.expect("dyn table");
        assert_eq!(
            &loaded[SLOT_COPY as usize..SLOT_COPY as usize + 8],
            &staged,
            "the definition's bytes must land at the COPY's offset"
        );
        assert_eq!(
            &loaded[SOURCE as usize..SOURCE as usize + 8],
            &staged,
            "a copy does not disturb its source"
        );
        assert_eq!(
            scopes,
            vec![Scope::All, Scope::ExcludeSelf],
            "the COPY must resolve in another object, not in the one it patches"
        );
    }

    #[test]
    fn an_unsupported_type_fails_the_load() {
        let mut file = file_image();
        wr64(&mut file, RELA_VA as usize + 8, 0xdead_beef);
        let mut loaded = vec![SENTINEL; FILE_LEN];
        assert_eq!(
            apply(&file, &mut loaded, RELA_VA, 4 * RELA_SIZE).0,
            Err(RelocError::Unsupported(0xdead_beef))
        );
    }

    #[test]
    fn a_target_outside_the_image_is_refused() {
        let mut file = file_image();
        wr64(&mut file, RELA_VA as usize, (FILE_LEN + 8) as u64);
        let mut loaded = vec![SENTINEL; FILE_LEN];
        assert_eq!(
            apply(&file, &mut loaded, RELA_VA, RELA_SIZE).0,
            Err(RelocError::OutOfRange(BASE + FILE_LEN as u64 + 8))
        );
    }

    #[test]
    fn an_undefined_name_fails_rather_than_binding_elsewhere() {
        // Repoint the PLT's JUMP_SLOT at symbol 3, "nowhere", which no object in
        // this test defines.
        let mut file = file_image();
        wr64(
            &mut file,
            PLT_VA as usize + 8,
            r_info(3, R_X86_64_JUMP_SLOT),
        );
        let mut loaded = vec![SENTINEL; FILE_LEN];
        let mut name = SymName::new();
        assert!(name.set(b"nowhere"));
        assert_eq!(
            apply(&file, &mut loaded, PLT_VA, RELA_SIZE).0,
            Err(RelocError::Unresolved(name))
        );
    }

    #[test]
    fn a_symbol_index_past_the_table_is_a_bad_symbol() {
        let mut file = file_image();
        wr64(
            &mut file,
            PLT_VA as usize + 8,
            r_info(NSYM as u32 + 5, R_X86_64_JUMP_SLOT),
        );
        let mut loaded = vec![SENTINEL; FILE_LEN];
        assert_eq!(
            apply(&file, &mut loaded, PLT_VA, RELA_SIZE).0,
            Err(RelocError::BadSymbol(NSYM as u32 + 5, SymError::Missing))
        );
    }

    #[test]
    fn a_table_size_that_is_not_whole_entries_is_refused() {
        let file = file_image();
        let mut loaded = vec![SENTINEL; FILE_LEN];
        assert_eq!(
            apply(&file, &mut loaded, RELA_VA, RELA_SIZE + 1).0,
            Err(RelocError::BadSize(RELA_SIZE + 1))
        );
    }

    #[test]
    fn a_name_that_does_not_fit_is_refused_rather_than_truncated() {
        let mut out = SymName::new();
        let long = [b'x'; SYMNAME_MAX + 1];
        assert!(!out.set(&long));
        assert_eq!(out.as_bytes(), b"");
        assert!(out.set(&[b'y'; SYMNAME_MAX]));
        assert_eq!(out.as_bytes().len(), SYMNAME_MAX);
    }
}
