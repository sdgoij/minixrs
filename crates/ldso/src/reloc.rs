//! The relocation rules the loader applies.
//!
//! [`reloc_action`] is the whole per-relocation decision and [`apply_table`] is
//! the walk that drives it. Both are kept apart from real memory so they are
//! unit-tested directly (`cargo test -p ldso`); what an image has to provide is
//! the small [`RelocImage`] surface — its relocation table, a symbol's name, and
//! a store — and the loader and the host tests each implement it over their own
//! idea of "the image".

use crate::elf::{
    EM_AARCH64, EM_RISCV, EM_X86_64, R_AARCH64_ABS64, R_AARCH64_COPY, R_AARCH64_GLOB_DAT,
    R_AARCH64_JUMP_SLOT, R_AARCH64_NONE, R_AARCH64_RELATIVE, R_AARCH64_TLS_DTPMOD64,
    R_AARCH64_TLSDESC, R_RISCV_64, R_RISCV_COPY, R_RISCV_JUMP_SLOT, R_RISCV_NONE, R_RISCV_RELATIVE,
    R_RISCV_TLS_DTPMOD64, R_X86_64_64, R_X86_64_COPY, R_X86_64_DTPMOD64, R_X86_64_GLOB_DAT,
    R_X86_64_JUMP_SLOT, R_X86_64_NONE, R_X86_64_RELATIVE, RELA_SIZE, Rela,
};

/// One target's relocation numbering.
///
/// ELF gives every machine its own type numbers, so which numbers mean "a
/// `RELATIVE` fixup" or "a GOT word" is part of the target's ABI and not
/// something the loader may guess at. Every value here comes from that
/// target's psABI header — the fork's LLVM carries them under
/// `ci-llvm/include/llvm/BinaryFormat/ELFRelocs/` — because a wrong number is a
/// wrong *write* into a loaded image, which surfaces much later as a crash in a
/// program that was linked correctly.
///
/// Only the types this loader implements are named. Anything else in a table is
/// [`Action::Unsupported`] and fails the load, which is the point: a silently
/// skipped relocation leaves an image half-built.
#[derive(Debug, Clone, Copy)]
pub struct Relocs {
    /// The machine, as the ELF header carries it (`e_machine`).
    pub e_machine: u16,
    /// The machine's name, for the loader's own diagnostics.
    pub name: &'static str,
    pub none: u32,
    /// An absolute address, `S + A`.
    pub abs64: u32,
    /// The loader's own fixup, `B + A`.
    pub relative: u32,
    /// A GOT/PLT word naming a symbol, `S + A`.
    pub glob_dat: u32,
    pub jump_slot: u32,
    /// Move the definition's bytes into the executable's reservation.
    pub copy: u32,
    /// Fill a `tls_index`'s module number, which is the loader's to say rather than the
    /// relocation's (see [`RelocImage::tls_module`]).
    pub dtpmod64: u32,
    /// A `TLSDESC` descriptor: a *pair* of words, the resolver and its argument,
    /// which a thread-local access calls instead of `__tls_get_addr` (AArch64's
    /// default dialect). [`Relocs::none`] on a target that has no such relocation.
    pub tlsdesc: u32,
}

pub const X86_64: Relocs = Relocs {
    e_machine: EM_X86_64,
    name: "x86_64",
    none: R_X86_64_NONE,
    abs64: R_X86_64_64,
    relative: R_X86_64_RELATIVE,
    glob_dat: R_X86_64_GLOB_DAT,
    jump_slot: R_X86_64_JUMP_SLOT,
    copy: R_X86_64_COPY,
    dtpmod64: R_X86_64_DTPMOD64,
    tlsdesc: R_X86_64_NONE,
};

/// RISC-V's own numbering. `abs64` and `glob_dat` are deliberately the same
/// type: the psABI has no `GLOB_DAT`, and `R_RISCV_64` fills both roles.
pub const RISCV64: Relocs = Relocs {
    e_machine: EM_RISCV,
    name: "riscv64",
    none: R_RISCV_NONE,
    abs64: R_RISCV_64,
    relative: R_RISCV_RELATIVE,
    glob_dat: R_RISCV_64,
    jump_slot: R_RISCV_JUMP_SLOT,
    copy: R_RISCV_COPY,
    dtpmod64: R_RISCV_TLS_DTPMOD64,
    tlsdesc: R_RISCV_NONE,
};

pub const AARCH64: Relocs = Relocs {
    e_machine: EM_AARCH64,
    name: "aarch64",
    none: R_AARCH64_NONE,
    abs64: R_AARCH64_ABS64,
    relative: R_AARCH64_RELATIVE,
    glob_dat: R_AARCH64_GLOB_DAT,
    jump_slot: R_AARCH64_JUMP_SLOT,
    copy: R_AARCH64_COPY,
    dtpmod64: R_AARCH64_TLS_DTPMOD64,
    tlsdesc: R_AARCH64_TLSDESC,
};

/// The table for the target this loader was built for.
#[cfg(target_arch = "x86_64")]
pub const TARGET: Relocs = X86_64;
#[cfg(target_arch = "riscv64")]
pub const TARGET: Relocs = RISCV64;
#[cfg(target_arch = "aarch64")]
pub const TARGET: Relocs = AARCH64;

/// What to do with one relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to write (a no-op relocation).
    Skip,
    /// Store this at `r_offset`.
    Write(u64),
    /// Fill a `tls_index`'s module number with the *owner's* ([`RelocImage::tls_module`]) —
    /// the one value in a thread-local access the layout decides rather than the object's own
    /// code.
    WriteModule,
    /// A byte copy (`COPY`): the walk resolves the symbol with
    /// [`Scope::ExcludeSelf`] and moves the definition's bytes to `r_offset`.
    Copy,
    /// Write a `TLSDESC` descriptor at `r_offset`: its resolver's address, then
    /// this argument, the variable's offset from the thread pointer.
    TlsDesc(u64),
    /// A relocation type this loader does not implement; the load must fail
    /// rather than continue with a half-relocated image.
    Unsupported,
}

/// Resolve one relocation. `base` is the loaded image's base address (the value
/// a `RELATIVE` fixup adds to); `sym_value` is the resolved symbol's address,
/// ignored by `RELATIVE` (and by `COPY`, whose bytes the walk moves itself).
pub fn reloc_action(typ: u32, base: u64, sym_value: u64, addend: i64) -> Action {
    action(TARGET, typ, base, sym_value, addend)
}

/// The same decision against an explicit table.
///
/// Separate from [`reloc_action`] so every target's numbering is testable on the
/// host: the constants are plain numbers, and a table is data, so one `cargo
/// test` covers all three machines without building for them.
///
/// The tests are comparisons rather than a `match` because RISC-V gives `abs64`
/// and `glob_dat` the same number, which a `match` would reject as an
/// unreachable arm.
pub fn action(r: Relocs, typ: u32, base: u64, sym_value: u64, addend: i64) -> Action {
    if typ == r.none {
        return Action::Skip;
    }
    if typ == r.relative {
        return Action::Write(base.wrapping_add(addend as u64));
    }
    if typ == r.abs64 || typ == r.glob_dat || typ == r.jump_slot {
        return Action::Write(sym_value.wrapping_add(addend as u64));
    }
    // A thread-local in the general- or local-dynamic model is reached through a
    // `tls_index` holding a module id and an offset within it, and the loader is
    // the only one that can say which module that is. The symbol's *address* is
    // not what goes here.
    if typ == r.dtpmod64 {
        return Action::WriteModule;
    }
    if typ == r.tlsdesc {
        // A thread-local access in the `TLSDESC` dialect asks a per-descriptor
        // resolver where its variable is, so the loader writes the descriptor.
        // For a variable the linker resolved itself the offset is the addend —
        // which is the form `R_AARCH64_TLSDESC` takes with no symbol, and what
        // LLD emits for a thread-local the object defines — and the walk refuses
        // the symbol's form, whose offset is the definition's (`apply_table`).
        return Action::TlsDesc(addend as u64);
    }
    // A non-PIE executable's reference to a variable defined in a shared object:
    // the linker reserved space for the variable in the executable, and the
    // loader has to move the object's initial value into it.
    if typ == r.copy {
        return Action::Copy;
    }
    Action::Unsupported
}

/// Whether a relocation type's *value* is a symbol's address. `COPY` is not one
/// of these: its bytes are moved by the walk, and its definition is looked up
/// with a narrower scope than a value's.
pub fn uses_symbol(typ: u32) -> bool {
    uses_symbol_in(TARGET, typ)
}

/// [`uses_symbol`] against an explicit table.
pub fn uses_symbol_in(r: Relocs, typ: u32) -> bool {
    typ == r.abs64 || typ == r.glob_dat || typ == r.jump_slot
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

/// Which pass of a lookup an object belongs to, or `None` when it is not visible at all.
///
/// A lookup is two passes over the loaded objects: the ones in the asking object's own
/// `dlopen` group first, and then the global scope. Load order decides within each pass.
///
/// That order is what makes an object's own definition of a name win over a global one of
/// the same name, and it is what keeps a `RTLD_LOCAL` object's symbols out of
/// `dlsym(RTLD_DEFAULT, …)` — the main program's group is 0 and a locally loaded object's is
/// not, and being local it is not in the global scope either. Everything mapped before the
/// program runs is group 0 *and* global, so for a startup load both passes cover the same
/// objects and the order makes no difference, which is why this changed nothing about it.
pub const fn lookup_pass(obj_group: u16, obj_global: bool, owner_group: u16) -> Option<u8> {
    if obj_group == owner_group {
        Some(0)
    } else if obj_global {
        Some(1)
    } else {
        None
    }
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
    /// A `TLSDESC` for a symbol another object defines. The port places one
    /// object's thread-local storage, so there is no block for the symbol's.
    TlsDescForASymbol,
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
    /// The runtime address of the loader's `TLSDESC` resolver for a
    /// thread-local it placed: the "static" one, which returns the descriptor's
    /// argument — the variable's offset from the thread pointer.
    fn tlsdesc_static(&self) -> u64;
    /// The module number this object's thread-locals belong to, which is what a `tls_index`'s
    /// module word has to name for `__tls_get_addr` to find the right storage. ELF numbers
    /// modules from 1; 0 means "no module", which is what an object with no `PT_TLS` answers.
    ///
    /// It is not the relocation's to state: the number depends on where the object was placed
    /// against the modules already loaded, so the walk defers to the image
    /// ([`Action::WriteModule`]).
    fn tls_module(&self) -> u64;
    /// Where this object's storage starts relative to the thread pointer — the layout's
    /// decision (`crate::layout::tls_layout`), which a `TLSDESC` argument needs because it is
    /// thread-pointer-relative while the offsets an object's own code carries are relative to
    /// its own storage.
    fn tls_disp(&self) -> i64;
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
            Action::WriteModule => {
                let at = base.wrapping_add(r.r_offset);
                let module = img.tls_module();
                if !img.store(at, module) {
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
            Action::TlsDesc(offset) => {
                // The symbol's form needs the defining object's storage, which a descriptor
                // does not carry — and it is the dialect this port's compiler does not emit for
                // a thread-local the object defines itself. Refused rather than half-answered.
                if r.sym() != 0 {
                    return Err(RelocError::TlsDescForASymbol);
                }
                let resolver = img.tlsdesc_static();
                // The linker's addend is the variable's offset from the thread pointer on the
                // assumption that the module starts *there* — module 0's placement. Every other
                // module's storage begins `disp` bytes along, so the descriptor has to say so.
                // This is the one relocation whose value the layout decides, which is why the
                // layout is computed before the walk.
                let arg = (offset as i64).wrapping_add(img.tls_disp()) as u64;
                let at = base.wrapping_add(r.r_offset);
                if !img.store(at, resolver) || !img.store(at + 8, arg) {
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
    /// Where the walk's `TLSDESC` resolver stands in for the loader's: the test
    /// image has no loader, so this is the address a descriptor must name.
    const TLS_RESOLVER: u64 = 0x99_0000;
    /// The module number the test image claims, and the placement it claims: the *first*
    /// module, at the thread pointer, which is what an object's own code assumes.
    const TLS_MODULE: u64 = 1;
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

        fn tlsdesc_static(&self) -> u64 {
            self.base + TLS_RESOLVER
        }

        /// The test image is the *first* module, at the thread pointer — the placement the
        /// single-module loader had, so the values these tests expect are the ones an object's
        /// own code would carry.
        fn tls_module(&self) -> u64 {
            TLS_MODULE
        }

        fn tls_disp(&self) -> i64 {
            0
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

    #[test]
    fn dtpmod_fills_the_module_id_not_the_symbol_value() {
        // The symbol's address, passed in as if the walk had resolved one, must
        // not reach the store: `__tls_get_addr` is asked for a module, and which module is
        // the image's to say, not the relocation's.
        assert_eq!(
            reloc_action(R_X86_64_DTPMOD64, BASE, 0x201_0000, 0),
            Action::WriteModule
        );
    }

    /// Every target's numbering resolves to the same rules.
    ///
    /// The tables are data, so this covers all three machines on the host: a
    /// wrong number in any of them is a wrong *write* into a loaded image, and
    /// this is where it has to fail instead.
    #[test]
    fn every_target_has_the_same_rules_in_its_own_numbering() {
        for r in [X86_64, RISCV64, AARCH64] {
            assert!(r.name == "x86_64" || r.name == "riscv64" || r.name == "aarch64");
            assert_eq!(
                action(r, r.none, BASE, 0xdead, 0),
                Action::Skip,
                "{}",
                r.name
            );
            assert_eq!(
                action(r, r.relative, BASE, 0, 0x1234),
                Action::Write(BASE + 0x1234),
                "{}",
                r.name
            );
            for t in [r.abs64, r.glob_dat, r.jump_slot] {
                assert_eq!(
                    action(r, t, BASE, 0x2004, 8),
                    Action::Write(0x200c),
                    "{} type {t:#x}",
                    r.name
                );
                assert!(uses_symbol_in(r, t), "{} type {t:#x}", r.name);
            }
            assert_eq!(
                action(r, r.dtpmod64, BASE, 0x2004, 0),
                Action::WriteModule,
                "{}",
                r.name
            );
            assert_eq!(action(r, r.copy, BASE, 0, 0), Action::Copy, "{}", r.name);
            // Only `abs64`/`glob_dat`/`jump_slot` take a symbol's address, and
            // only the types above are implemented: anything else fails the load
            // rather than leaving the image half-built.
            assert!(!uses_symbol_in(r, r.copy), "{}", r.name);
            assert!(!uses_symbol_in(r, r.relative), "{}", r.name);
            assert_eq!(
                action(r, 0xdead_beef, BASE, 0, 0),
                Action::Unsupported,
                "{}",
                r.name
            );
        }
    }

    /// The `TLSDESC` dialect, which only AArch64's table names: the descriptor's
    /// argument is the offset the linker resolved into the addend, and the walk
    /// pairs it with the loader's own resolver — so this type must not be one the
    /// walk resolves a name for.
    #[test]
    fn a_tlsdesc_descriptor_is_the_resolver_and_the_linkers_offset() {
        assert_eq!(AARCH64.tlsdesc, R_AARCH64_TLSDESC);
        assert_eq!(
            action(AARCH64, AARCH64.tlsdesc, BASE, 0xdead, 0x18),
            Action::TlsDesc(0x18)
        );
        assert!(!uses_symbol_in(AARCH64, AARCH64.tlsdesc));
        // The other two targets have no such relocation, and say so with `none`:
        // the no-op check comes first, so such a type is skipped rather than
        // writing a descriptor.
        assert_eq!(X86_64.tlsdesc, X86_64.none);
        assert_eq!(RISCV64.tlsdesc, RISCV64.none);
        assert_eq!(action(X86_64, X86_64.tlsdesc, BASE, 0, 8), Action::Skip);
    }

    /// The two new tables' numbers, pinned against their psABI headers as the
    /// fork's LLVM carries them (`ELFRelocs/RISCV.def`, `ELFRelocs/AArch64.def`)
    /// and against `e_machine` as `elf.h` has it. A drift here would otherwise
    /// show up as a wrong write in a guest and nowhere else.
    #[test]
    fn the_new_targets_numbers_are_the_psabis() {
        assert_eq!(RISCV64.e_machine, 243);
        assert_eq!(
            (
                RISCV64.none,
                RISCV64.abs64,
                RISCV64.glob_dat,
                RISCV64.relative,
                RISCV64.copy,
                RISCV64.jump_slot,
                RISCV64.dtpmod64
            ),
            // RISC-V has no GLOB_DAT; `R_RISCV_64` fills both roles.
            (0, 2, 2, 3, 4, 5, 7)
        );
        assert_eq!(AARCH64.e_machine, 183);
        assert_eq!(
            (
                AARCH64.none,
                AARCH64.abs64,
                AARCH64.relative,
                AARCH64.copy,
                AARCH64.glob_dat,
                AARCH64.jump_slot,
                AARCH64.dtpmod64
            ),
            (0, 0x101, 0x403, 0x400, 0x401, 0x402, 0x404)
        );
        assert_eq!(AARCH64.tlsdesc, 0x407);
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

    /// The two-pass rule, and the half of it that is a scope rather than an order: an object
    /// in the asking object's own group is found before the global scope, and one that is
    /// neither in that group nor global — a `RTLD_LOCAL` load — is not found at all. That is
    /// the whole of what `RTLD_LOCAL` and `RTLD_GLOBAL` mean here, and it is what makes
    /// `dlsym(RTLD_DEFAULT, …)` miss a locally loaded object.
    #[test]
    fn a_lookup_is_the_group_before_the_global_scope() {
        // The asking object is in group 2.
        assert_eq!(lookup_pass(2, false, 2), Some(0));
        assert_eq!(lookup_pass(0, true, 2), Some(1));
        assert_eq!(lookup_pass(3, false, 2), None);
        // The startup graph is group 0 *and* global, so both passes are the same objects for
        // it and the order cannot matter — which is why adding this changed no startup load.
        assert_eq!(lookup_pass(0, true, 0), Some(0));
    }
}
