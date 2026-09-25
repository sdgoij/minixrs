//! The runtime linker.
//!
//! The loader is entered as a program's `PT_INTERP` interpreter. The kernel has
//! mapped the main program's ELF header page read-only and put its address in
//! the register `_start` reads (`r9`); from there the loader takes `e_entry` and
//! the program headers, maps each `DT_NEEDED` object, resolves the symbols those
//! objects (and the main program) need, applies every object's `RELATIVE`
//! fixups, patches the main program's PLT/GOT, and returns `e_entry` for its
//! caller to enter.
//!
//! Phase 1 is PIC: each `DT_NEEDED` is an `ET_DYN` object mapped at a base the
//! allocator picks (deterministically — the port has no ASLR), and its own
//! `RELATIVE` relocations are applied against that base. The main program is
//! still non-PIE: its addresses are absolute, so its bias is 0.
//!
//! A `DT_NEEDED` list is followed transitively, depth-first, so the loaded
//! objects come out in reverse topological order (a dependency precedes what
//! needs it, which is the order the initialisers then run in backwards). Eager
//! binding throughout: no resolve trampoline, so an unresolved symbol is a load
//! failure rather than a first-call fault.
//!
//! Two things here are the loader's own rather than any object's. Its symbol
//! table answers the names a position-independent object cannot resolve for
//! itself — the TLS bounds a `cdylib` is linked without, and `__tls_get_addr`
//! ([`loader_defined`]) — and it installs the thread's thread-local storage
//! before any initialiser can run, because an initialiser may touch one
//! ([`install_tls`]).

use crate::elf::{
    DT_HASH, DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTREL,
    DT_PLTRELSZ, DT_RELA, DT_RELASZ, DT_STRTAB, DT_SYMTAB, DYN_SIZE, EM_X86_64, ET_DYN, Elf,
    MAX_DYN, PF_W, PF_X, PT_LOAD, RELA_SIZE, Rela, SYM_SIZE, Sym,
};
use crate::layout::{BaseAlloc, image_extent, page_down, page_up, tls_block_size};
use crate::reloc::{
    Def, RelocError, RelocImage, SYMNAME_MAX, Scope, SymError, SymName, TLS_MODULE_ID, apply_table,
};
use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

/// A `DT_NEEDED` name is looked up here, in order.
const SEARCH_PATH: &[&[u8]] = &[b"/lib/", b"/usr/lib/"];

const PAGE: u64 = 0x1000;
const MAX_OBJECTS: usize = 8;
/// Longest library name kept for the "already loaded" check.
const NAME_MAX: usize = 128;

fn write_bytes(b: &[u8]) {
    unsafe { minix_rt::write(2, b.as_ptr(), b.len()) };
}

fn write_dec(mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    write_bytes(&buf[i..]);
}

fn write_hex(v: u64) {
    let mut buf = [b'0'; 16];
    for (i, b) in buf.iter_mut().enumerate() {
        let nib = ((v >> (4 * (15 - i))) & 0xf) as u8;
        *b = if nib < 10 {
            b'0' + nib
        } else {
            b'a' + nib - 10
        };
    }
    write_bytes(&buf);
}

fn die(msg: &[u8]) -> ! {
    write_bytes(msg);
    minix_rt::exit(1)
}

fn die_parts(parts: &[&[u8]]) -> ! {
    for p in parts {
        write_bytes(p);
    }
    minix_rt::exit(1)
}

/// Why a load failed. Every one is fatal: a half-relocated program must not run.
enum LoadError {
    NotElf,
    NotObject,
    NoDynamic,
    TwoTlsModules,
    NoTlsSpace,
    TooManyObjects,
    CannotLoad,
    NoSpace,
    DtRel,
    Reloc(RelocError),
}

fn die_load(e: LoadError) -> ! {
    match e {
        LoadError::NotElf => die(b"ld.so: main header page is not an x86_64 ELF\n"),
        LoadError::NotObject => die(b"ld.so: a DT_NEEDED is not a PIC object\n"),
        LoadError::NoDynamic => die(b"ld.so: no PT_DYNAMIC\n"),
        LoadError::TwoTlsModules => {
            die(b"ld.so: more than one loaded object has thread-local storage\n")
        }
        LoadError::NoTlsSpace => die(b"ld.so: no room for a thread-local block\n"),
        LoadError::TooManyObjects => die(b"ld.so: too many objects\n"),
        LoadError::CannotLoad => die(b"ld.so: cannot load DT_NEEDED\n"),
        LoadError::NoSpace => die(b"ld.so: no room for another object\n"),
        LoadError::DtRel => die(b"ld.so: PLT relocations are not RELA\n"),
        LoadError::Reloc(RelocError::BadSize(_)) => die(b"ld.so: malformed RELA table size\n"),
        LoadError::Reloc(RelocError::BadTable(..)) => {
            die(b"ld.so: a relocation table is not in the image\n")
        }
        LoadError::Reloc(RelocError::Unsupported(t)) => {
            write_bytes(b"ld.so: unsupported relocation type ");
            write_dec(t as u64);
            die(b"\n")
        }
        LoadError::Reloc(RelocError::BadSymbol(i, SymError::Missing)) => {
            write_bytes(b"ld.so: no name for symbol ");
            write_dec(i as u64);
            die(b"\n")
        }
        LoadError::Reloc(RelocError::BadSymbol(i, SymError::TooLong)) => {
            write_bytes(b"ld.so: symbol ");
            write_dec(i as u64);
            die(b" has too long a name\n")
        }
        LoadError::Reloc(RelocError::Unresolved(n)) => {
            die_parts(&[b"ld.so: unresolved symbol ", n.as_bytes(), b"\n"])
        }
        LoadError::Reloc(RelocError::OutOfRange(a)) => {
            write_bytes(b"ld.so: relocation target outside the image: 0x");
            write_hex(a);
            die(b"\n")
        }
    }
}

unsafe fn rd_u8(va: u64) -> u8 {
    unsafe { ptr::read(va as *const u8) }
}

unsafe fn rd_u16(va: u64) -> u16 {
    unsafe { ptr::read_unaligned(va as *const u16) }
}

unsafe fn rd_u32(va: u64) -> u32 {
    unsafe { ptr::read_unaligned(va as *const u32) }
}

unsafe fn rd_u64(va: u64) -> u64 {
    unsafe { ptr::read_unaligned(va as *const u64) }
}

unsafe fn wr_u64(va: u64, v: u64) {
    unsafe { ptr::write_unaligned(va as *mut u64, v) }
}

/// A NUL-terminated string at `va`, reading at most [`SYMNAME_MAX`] bytes — a
/// longer one is reported as too long rather than scanned into unmapped memory.
unsafe fn cstr_va(va: u64) -> &'static [u8] {
    let mut n = 0usize;
    while n < SYMNAME_MAX && unsafe { rd_u8(va + n as u64) } != 0 {
        n += 1;
    }
    unsafe { core::slice::from_raw_parts(va as *const u8, n) }
}

/// The `DT_NEEDED` name an object was loaded by, so a second request for the same
/// library does not map it again. Bytes, because the string it came from lived in
/// a file the loader has since closed.
#[derive(Clone, Copy)]
struct LibName {
    buf: [u8; NAME_MAX],
    len: u8,
}

impl LibName {
    const EMPTY: Self = Self {
        buf: [0; NAME_MAX],
        len: 0,
    };

    /// Copy `name` in. `false` when it does not fit — [`load`] has already refused
    /// names that long, so this cannot happen for a name that reached a load.
    fn set(&mut self, name: &[u8]) -> bool {
        if name.len() > NAME_MAX {
            return false;
        }
        self.buf[..name.len()].copy_from_slice(name);
        self.len = name.len() as u8;
        true
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

/// An object's thread-local storage, as its `PT_TLS` segment states it, moved to
/// runtime addresses.
#[derive(Clone, Copy)]
struct TlsSlot {
    /// Runtime VA of the initialised image (`p_vaddr + bias`), which lies inside
    /// one of the object's mapped `PT_LOAD`s.
    init: u64,
    /// How many bytes of it are initialised (`p_filesz`); the rest of the block
    /// is zeroed.
    init_len: u64,
    /// How many bytes every thread's copy needs (`p_memsz`).
    memsz: u64,
}

/// One loaded ELF object: the link-time facts its dynamic array states, and the
/// bias they were mapped at (`0` for a non-PIE main program). Every read adds
/// the bias; `img_start`/`img_end` are the runtime addresses a relocation
/// belonging to this object may write.
#[derive(Clone, Copy)]
struct Object {
    /// Link-time VA of this object's dynamic array.
    dynamic: u64,
    bias: u64,
    /// Link-time VA of its symbol table and string table.
    symtab: u64,
    strtab: u64,
    nsym: usize,
    img_start: u64,
    img_end: u64,
    /// The thread-local storage it brings with it, if any.
    tls: Option<TlsSlot>,
    /// The name it was loaded by, empty for the main program.
    name: LibName,
}

impl Object {
    const EMPTY: Object = Object {
        dynamic: 0,
        bias: 0,
        symtab: 0,
        strtab: 0,
        nsym: 0,
        img_start: 0,
        img_end: 0,
        tls: None,
        name: LibName::EMPTY,
    };

    /// The name of dynamic symbol `idx`, if it is a defined global or weak.
    unsafe fn defined_name(&self, idx: u32) -> Option<&'static [u8]> {
        if idx as usize >= self.nsym {
            return None;
        }
        let st = self.bias + self.symtab + (idx as u64) * SYM_SIZE as u64;
        let st_name = unsafe { rd_u32(st) };
        let st_shndx = unsafe { rd_u16(st + 6) };
        if st_name == 0 || st_shndx == 0 {
            return None;
        }
        let bind = unsafe { rd_u8(st + 4) } >> 4;
        if bind != 1 && bind != 2 {
            // STB_GLOBAL, STB_WEAK
            return None;
        }
        Some(unsafe { cstr_va(self.bias + self.strtab + st_name as u64) })
    }

    /// The value of the first `tag` in this object's dynamic array.
    unsafe fn dyn_tag(&self, tag: i64) -> Option<u64> {
        let mut p = self.bias + self.dynamic;
        for _ in 0..MAX_DYN {
            let t = unsafe { rd_u64(p) } as i64;
            if t == DT_NULL {
                return None;
            }
            if t == tag {
                return Some(unsafe { rd_u64(p + 8) });
            }
            p = p.checked_add(DYN_SIZE as u64)?;
        }
        None
    }

    /// The dynamic symbol count, bounded by what the object has room for.
    ///
    /// The count is the `.hash` table's `nchain` (ELF requires `DT_HASH`
    /// alongside `DT_GNU_HASH` for compatibility, and LLD emits both). An object
    /// the static linker gave no dynamic symbols claims none — which is how a
    /// non-PIE program contributes to a lookup only the names it really exports.
    ///
    /// `nchain` is the one number here that a malformed image could inflate, and
    /// a scan is only ever as good as its bound, so the claim is clamped to the
    /// symbols that fit between the table and the end of the object.
    unsafe fn sym_count(&self) -> usize {
        if self.symtab == 0 || self.strtab == 0 {
            return 0;
        }
        let Some(h) = (unsafe { self.dyn_tag(DT_HASH) }) else {
            return 0;
        };
        let claimed = unsafe { rd_u32(self.bias + h + 4) } as u64;
        let symtab = self.bias + self.symtab;
        let room = self.img_end.saturating_sub(symtab) / SYM_SIZE as u64;
        claimed.min(room) as usize
    }
}

/// The definition of `name` in some loaded object, as that object states it.
///
/// `owner` is the object being relocated: [`Scope::ExcludeSelf`] skips it, which a
/// COPY needs — this image's own symbol for the name is the destination it is
/// about to write, so resolving there would copy the destination onto itself.
unsafe fn find_symbol(objects: &[Object], name: &[u8], scope: Scope, owner: usize) -> Option<Def> {
    // The loader is linkable to the objects it maps, the way a system `ld.so` is:
    // the names below belong to the loader and are in no object's tables.
    if let Some(def) = loader_defined(objects, owner, name) {
        return Some(def);
    }
    for (i, o) in objects.iter().enumerate() {
        if scope == Scope::ExcludeSelf && i == owner {
            continue;
        }
        for idx in 0..o.nsym as u32 {
            if let Some(n) = unsafe { o.defined_name(idx) }
                && n == name
            {
                let st = o.bias + o.symtab + (idx as u64) * SYM_SIZE as u64;
                let value = o.bias + unsafe { rd_u64(st + 8) };
                let size = unsafe { rd_u64(st + 16) };
                // A definition outside the object that states it is not one to
                // trust: a value would point out of the object and a COPY would
                // move bytes from somewhere else.
                if value < o.img_start || value.checked_add(size).is_none_or(|e| e > o.img_end) {
                    return None;
                }
                return Some(Def { addr: value, size });
            }
        }
    }
    None
}

/// A symbol the loader itself defines for the objects it loads.
///
/// A position-independent object reaches its own thread-local storage through
/// `__tls_get_addr`, and its buffer allocator through `__tls_start`,
/// `__tdata_end` and `__tls_end` — the bounds a *statically linked* program gets
/// from the port's linker script. A `cdylib` is linked by nothing but LLD, so it
/// leaves those three undefined, and the loader answers with the object's own
/// `PT_TLS`, which is where those bounds really come from anyway.
fn loader_defined(objects: &[Object], owner: usize, name: &[u8]) -> Option<Def> {
    if name == b"__tls_get_addr" {
        return Some(Def {
            addr: __tls_get_addr as *const () as u64,
            size: 0,
        });
    }
    let tls = objects.get(owner)?.tls?;
    let addr = if name == b"__tls_start" {
        tls.init
    } else if name == b"__tdata_end" {
        tls.init + tls.init_len
    } else if name == b"__tls_end" {
        tls.init + tls.memsz
    } else {
        return None;
    };
    Some(Def { addr, size: 0 })
}

/// The image a relocation walk writes to: one loaded object's tables, read
/// through the real mapping, bounded to that object's own span of memory.
struct GuestImage {
    base: u64,
    symtab: u64,
    strtab: u64,
    nsym: usize,
    start: u64,
    end: u64,
}

impl GuestImage {
    fn of(obj: &Object) -> Self {
        Self {
            base: obj.bias,
            symtab: obj.symtab,
            strtab: obj.strtab,
            nsym: obj.nsym,
            start: obj.img_start,
            end: obj.img_end,
        }
    }
}

impl RelocImage for GuestImage {
    fn rela(&self, table_va: u64, index: usize) -> Option<Rela> {
        let p = self
            .base
            .checked_add(table_va)?
            .checked_add((index * RELA_SIZE) as u64)?;
        Some(Rela {
            r_offset: unsafe { rd_u64(p) },
            r_info: unsafe { rd_u64(p + 8) },
            r_addend: unsafe { rd_u64(p + 16) } as i64,
        })
    }

    fn sym_name(&self, idx: u32, out: &mut SymName) -> Result<(), SymError> {
        if idx as usize >= self.nsym {
            return Err(SymError::Missing);
        }
        let st = self.base + self.symtab + (idx as u64) * SYM_SIZE as u64;
        let st_name = unsafe { rd_u32(st) };
        if st_name == 0 {
            return Err(SymError::Missing);
        }
        let name = unsafe { cstr_va(self.base + self.strtab + st_name as u64) };
        // `cstr_va` stops at `SYMNAME_MAX` without seeing a NUL, so a name that
        // long is one that might be longer still.
        if name.len() == SYMNAME_MAX || !out.set(name) {
            return Err(SymError::TooLong);
        }
        Ok(())
    }

    fn sym_is_weak(&self, idx: u32) -> bool {
        if idx as usize >= self.nsym {
            return false;
        }
        let st = self.base + self.symtab + (idx as u64) * SYM_SIZE as u64;
        (unsafe { rd_u8(st + 4) }) >> 4 == Sym::STB_WEAK
    }

    fn bias(&self) -> u64 {
        self.base
    }

    fn store(&mut self, va: u64, value: u64) -> bool {
        if va < self.start || va.checked_add(8).is_none_or(|e| e > self.end) {
            return false;
        }
        unsafe { wr_u64(va, value) };
        true
    }

    fn copy_range(&mut self, dst: u64, src: u64, len: u64) -> bool {
        if dst < self.start || dst.checked_add(len).is_none_or(|e| e > self.end) {
            return false;
        }
        unsafe { ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, len as usize) };
        true
    }
}

/// Map one shared object's segments at `base` and read its dynamic tables.
fn read_and_map(fd: i32, alloc: &mut BaseAlloc) -> Result<Object, LoadError> {
    let mut hdr = [0u8; PAGE as usize];
    let n = minix_rt::read(fd, &mut hdr);
    if n < 64 {
        return Err(LoadError::NotObject);
    }
    let elf = Elf::new(&hdr[..n as usize]).map_err(|_| LoadError::NotObject)?;
    if elf.e_machine() != EM_X86_64 || elf.e_type() != ET_DYN {
        return Err(LoadError::NotObject);
    }

    let (lo, hi) = image_extent(&elf).ok_or(LoadError::NotObject)?;
    // The span to reserve is `hi`, not `hi - lo`: the object is mapped at
    // `base + page_down(p_vaddr)`, so an object linked at a non-zero base reaches
    // that much further past the base it was handed — and `DSO_LIMIT` has to be
    // checked against where it really ends.
    let base = alloc.reserve(hi).ok_or(LoadError::NoSpace)?;

    for i in 0..elf.e_phnum() as usize {
        let p = elf.phdr(i).ok_or(LoadError::NotObject)?;
        if p.p_type != PT_LOAD || p.p_memsz == 0 {
            continue;
        }
        let off = page_down(p.p_offset);
        let va = base + page_down(p.p_vaddr);
        let head = p.p_vaddr - page_down(p.p_vaddr);
        let len = page_up(head + p.p_memsz);
        let mut prot = minix_rt::vmem::PROT_READ;
        if p.p_flags & PF_W != 0 {
            prot |= minix_rt::vmem::PROT_WRITE;
        }
        if p.p_flags & PF_X != 0 {
            prot |= minix_rt::vmem::PROT_EXEC;
        }
        let r = unsafe {
            minix_rt::vmem::mmap(
                va as *mut u8,
                len as usize,
                prot,
                minix_rt::vmem::MAP_PRIVATE | minix_rt::vmem::MAP_FIXED,
                fd,
                off as i64,
            )
        };
        if r.is_null() || r as u64 != va {
            return Err(LoadError::NotObject);
        }
    }

    // What the object needs a thread of its own for, if anything. `PT_TLS`
    // points inside a `PT_LOAD`, so the initialised image is already mapped at
    // `base`; only the zeroed tail has to be made up when a thread's copy is
    // built (`install_tls`).
    let tls = elf.tls_phdr().filter(|p| p.p_memsz != 0).map(|p| TlsSlot {
        init: base + p.p_vaddr,
        init_len: p.p_filesz,
        memsz: p.p_memsz,
    });

    let dynp = elf.dynamic_phdr().ok_or(LoadError::NoDynamic)?;
    let mut obj = Object {
        dynamic: dynp.p_vaddr,
        bias: base,
        symtab: 0,
        strtab: 0,
        nsym: 0,
        img_start: base + lo,
        img_end: base + hi,
        tls,
        name: LibName::EMPTY,
    };
    obj.symtab = unsafe { obj.dyn_tag(DT_SYMTAB) }.ok_or(LoadError::NoDynamic)?;
    obj.strtab = unsafe { obj.dyn_tag(DT_STRTAB) }.ok_or(LoadError::NoDynamic)?;
    obj.nsym = unsafe { obj.sym_count() };
    Ok(obj)
}

/// Open a `DT_NEEDED` name from the search path and map it.
fn load(name: &[u8], alloc: &mut BaseAlloc) -> Result<Object, LoadError> {
    if name.len() + 16 > 128 {
        return Err(LoadError::CannotLoad);
    }
    let mut path = [0u8; 128];
    for dir in SEARCH_PATH {
        let total = dir.len() + name.len();
        path[..dir.len()].copy_from_slice(dir);
        path[dir.len()..total].copy_from_slice(name);
        let fd = minix_rt::open(&path[..total], 0);
        if fd < 0 {
            continue;
        }
        let fd = fd as i32;
        let obj = read_and_map(fd, alloc);
        minix_rt::close(fd);
        let mut obj = obj?;
        obj.name.set(name);
        return Ok(obj);
    }
    Err(LoadError::CannotLoad)
}

/// Relocate one object: its own `RELATIVE`/`R_*_64` tables, then its PLT.
fn relocate(objects: &[Object], owner_idx: usize) -> Result<(), LoadError> {
    let owner = objects[owner_idx];
    let mut img = GuestImage::of(&owner);
    let mut resolve =
        |name: &[u8], scope: Scope| unsafe { find_symbol(objects, name, scope, owner_idx) };

    if let Some(rela) = unsafe { owner.dyn_tag(DT_RELA) } {
        let sz = unsafe { owner.dyn_tag(DT_RELASZ).unwrap_or(0) };
        apply_table(&mut img, rela, sz as usize, &mut resolve).map_err(LoadError::Reloc)?;
    }
    if let Some(jmprel) = unsafe { owner.dyn_tag(DT_JMPREL) } {
        let sz = unsafe { owner.dyn_tag(DT_PLTRELSZ).unwrap_or(0) };
        let kind = unsafe { owner.dyn_tag(DT_PLTREL).unwrap_or(DT_RELA as u64) };
        if kind != DT_RELA as u64 {
            return Err(LoadError::DtRel);
        }
        apply_table(&mut img, jmprel, sz as usize, &mut resolve).map_err(LoadError::Reloc)?;
    }
    Ok(())
}

/// `obj`'s `DT_NEEDED` names, copied out: the walk that follows them changes the
/// object list, so they cannot be borrowed from it.
unsafe fn needed_names(obj: &Object, out: &mut [LibName; MAX_OBJECTS]) -> usize {
    let mut n = 0usize;
    let mut p = obj.bias + obj.dynamic;
    for _ in 0..MAX_DYN {
        let t = unsafe { rd_u64(p) } as i64;
        if t == DT_NULL {
            break;
        }
        if t == DT_NEEDED && n < out.len() {
            let name = unsafe { cstr_va(obj.bias + obj.strtab + rd_u64(p + 8)) };
            if out[n].set(name) {
                n += 1;
            }
        }
        p += DYN_SIZE as u64;
    }
    n
}

/// Load `owner`'s `DT_NEEDED` objects depth-first, appending each one *before*
/// following its own dependencies. That makes the list a reverse topological
/// order — every object appears before the objects it needs — and it is what
/// makes a library graph load once: a name already in the list is skipped, so a
/// diamond (two objects naming one library) or a cycle terminates instead of
/// filling the list. Relocations do not depend on the order, because a lookup
/// searches every object; the initialisers do, and walk the list backwards.
unsafe fn load_dependencies(
    objects: &mut [Object; MAX_OBJECTS],
    count: &mut usize,
    alloc: &mut BaseAlloc,
    owner: usize,
) {
    let mut names = [LibName::EMPTY; MAX_OBJECTS];
    let n = unsafe { needed_names(&objects[owner], &mut names) };
    for name in names.iter().take(n) {
        if objects[..*count]
            .iter()
            .any(|o| o.name.as_bytes() == name.as_bytes())
        {
            continue;
        }
        if *count == MAX_OBJECTS {
            die_load(LoadError::TooManyObjects);
        }
        let obj = match load(name.as_bytes(), alloc) {
            Ok(o) => o,
            Err(e) => die_load(e),
        };
        let idx = *count;
        objects[idx] = obj;
        *count += 1;
        unsafe { load_dependencies(objects, count, alloc, idx) };
    }
}

/// The arguments an initialiser is called with: the ELF ABI passes a constructor
/// the same three the program's entry point receives.
struct InitCtx {
    argc: u64,
    argv: u64,
    envp: u64,
}

/// Call a function the object named as an initialiser. It is the object's own
/// code, relocated by now and mapped executable.
unsafe fn call_init(f: u64, ctx: &InitCtx) {
    let f: unsafe extern "C" fn(u64, u64, u64) = unsafe { core::mem::transmute(f) };
    unsafe { f(ctx.argc, ctx.argv, ctx.envp) };
}

/// Run an object's initialisers: `DT_INIT` first, then `DT_INIT_ARRAY`, the order
/// `_dl_init` uses.
unsafe fn run_initialisers(obj: &Object, ctx: &InitCtx) {
    if let Some(f) = unsafe { obj.dyn_tag(DT_INIT) }
        && f != 0
    {
        unsafe { call_init(f, ctx) };
    }
    if let Some(array) = unsafe { obj.dyn_tag(DT_INIT_ARRAY) } {
        let sz = unsafe { obj.dyn_tag(DT_INIT_ARRAYSZ).unwrap_or(0) };
        for i in 0..(sz as usize) / 8 {
            let f = unsafe { rd_u64(obj.bias + array + (i as u64) * 8) };
            if f != 0 {
                unsafe { call_init(f, ctx) };
            }
        }
    }
}

/// The size of the thread-local block `__tls_get_addr` stands on, or 0 when no
/// object brought thread-local storage.
///
/// Written once before any object code can run and read by every thread's TLS
/// access, so it is an atomic rather than a plain static: the store is a load-
/// time event and the loads are arbitrary program points.
static TLS_BLOCK_SIZE: AtomicU64 = AtomicU64::new(0);

/// The pair the general- and local-dynamic TLS models hand to `__tls_get_addr`:
/// which module, and where in it.
#[repr(C)]
struct TlsIndex {
    module: u64,
    offset: u64,
}

/// The calling thread's thread pointer (the FS base).
///
/// There is no unprivileged way to read the FS base on this CPU, but the port's
/// own convention supplies one: the runtime's `tls_block_alloc` writes the
/// pointer at `[tp]`, and [`install_tls`] does the same, so that word *is* the
/// pointer. Reading it per call is what lets a thread `pthread_create` started
/// work — its block is its own, and only the distance from the pointer down to
/// the storage is fixed.
fn read_tp() -> u64 {
    let tp: u64;
    unsafe {
        asm!(
            "mov {tp}, qword ptr fs:[0]",
            tp = out(reg) tp,
            options(nostack, nomem, preserves_flags),
        )
    };
    tp
}

/// `__tls_get_addr`: the storage base of the module `ti` names, for *this*
/// thread.
///
/// A position-independent object reaches its thread-locals through this call in
/// the general- and local-dynamic models, because the answer depends on which
/// thread is asking. `ti.offset` is zero in the local-dynamic form — the object
/// adds the variable's offset itself, through a `DTPOFF32` the static linker
/// resolved — so adding it is what makes the general-dynamic form work too.
#[unsafe(no_mangle)]
unsafe extern "C" fn __tls_get_addr(ti: *const TlsIndex) -> *mut u8 {
    let size = TLS_BLOCK_SIZE.load(Ordering::Relaxed);
    if size == 0 {
        return ptr::null_mut();
    }
    let Some(ti) = (unsafe { ti.as_ref() }) else {
        return ptr::null_mut();
    };
    if ti.module != TLS_MODULE_ID {
        die(b"ld.so: __tls_get_addr asked for a module this load did not place\n");
    }
    read_tp().wrapping_sub(size).wrapping_add(ti.offset) as *mut u8
}

/// Install the calling thread's storage for the loaded objects' thread-locals.
///
/// The port has one thread pointer with one block behind it — the runtime's
/// `tls_block_alloc` builds the same block for a thread `pthread_create` starts,
/// and that is what makes the two agree: the loader hands the block size to
/// `__tls_get_addr` through [`TLS_BLOCK_SIZE`], and the runtime reaches the same
/// addresses by rounding the object's `p_memsz` the same way.
///
/// One object can be placed, at `tp - align16(p_memsz)`. That is where a non-PIE
/// program's `%fs`-relative offsets already expect its own thread-locals to be,
/// which is why the program is placed like any other object rather than treated
/// as a special case. Two objects with thread-locals cannot be: the loader gives
/// `__tls_get_addr` one offset to work from, so a second module's storage would
/// alias the first's — refused rather than allowed to read the wrong storage.
unsafe fn install_tls(objects: &[Object]) -> Result<(), LoadError> {
    let mut slot = None;
    for o in objects {
        let Some(tls) = o.tls else { continue };
        if slot.is_some() {
            return Err(LoadError::TwoTlsModules);
        }
        slot = Some(tls);
    }
    let Some(tls) = slot else { return Ok(()) };

    let size = tls_block_size(tls.memsz);
    // Room for the block and for the word the thread pointer sits on, which is
    // at its 16-aligned end.
    let alloc = unsafe { minix_rt::sbrk((size + 32) as isize) };
    if alloc < 0 {
        return Err(LoadError::NoTlsSpace);
    }
    let block = ((alloc as u64) + 15) & !15;
    unsafe {
        ptr::copy_nonoverlapping(
            tls.init as *const u8,
            block as *mut u8,
            tls.init_len as usize,
        );
        ptr::write_bytes(
            (block + tls.init_len) as *mut u8,
            0,
            (size - tls.init_len) as usize,
        );
        ptr::write((block + size) as *mut u64, block + size);
    }
    TLS_BLOCK_SIZE.store(size, Ordering::Relaxed);
    minix_rt::thread_set_tls((block + size) as usize);
    Ok(())
}

/// Load every object the main program needs, relocate everything, run every
/// initialiser, and return the main program's entry point.
///
/// # Safety
///
/// `main_hdr` must be the VA of the main program's ELF header page, mapped
/// read-only by VFS (the kernel passes it in the loader's entry register), and
/// `argc`/`argv`/`envp` the program's own.
pub unsafe fn run(argc: u64, argv: u64, envp: u64, main_hdr: u64) -> u64 {
    let hdr = unsafe { core::slice::from_raw_parts(main_hdr as *const u8, PAGE as usize) };
    let elf = match Elf::new(hdr) {
        Ok(e) => e,
        Err(_) => die_load(LoadError::NotElf),
    };
    if elf.e_machine() != EM_X86_64 {
        die_load(LoadError::NotElf);
    }
    let entry = elf.e_entry();
    // Non-PIE: the main program's addresses are absolute, so its bias is 0 and
    // its link-time tables read exactly where the image says.
    let (main_lo, main_hi) = match image_extent(&elf) {
        Some(e) => e,
        None => die_load(LoadError::NotObject),
    };
    let dynp = match elf.dynamic_phdr() {
        Some(p) => p,
        None => die_load(LoadError::NoDynamic),
    };

    // Thread-locals of the program's own, if it has any. Its bias is 0, so the
    // segment's link-time address is its runtime address. [`install_tls`] has to
    // see them to refuse them: the program's `%fs`-relative offsets are already
    // baked into its code by the static linker.
    let tls = elf.tls_phdr().filter(|p| p.p_memsz != 0).map(|p| TlsSlot {
        init: p.p_vaddr,
        init_len: p.p_filesz,
        memsz: p.p_memsz,
    });

    let mut main = Object {
        dynamic: dynp.p_vaddr,
        bias: 0,
        symtab: 0,
        strtab: 0,
        nsym: 0,
        img_start: main_lo,
        img_end: main_hi,
        tls,
        name: LibName::EMPTY,
    };
    main.symtab = match unsafe { main.dyn_tag(DT_SYMTAB) } {
        Some(v) => v,
        None => 0,
    };
    main.strtab = match unsafe { main.dyn_tag(DT_STRTAB) } {
        Some(v) => v,
        None => 0,
    };
    main.nsym = unsafe { main.sym_count() };

    let mut objects = [Object::EMPTY; MAX_OBJECTS];
    objects[0] = main;
    let mut nobj = 1usize;
    let mut alloc = BaseAlloc::new();
    unsafe { load_dependencies(&mut objects, &mut nobj, &mut alloc, 0) };

    let loaded = &objects[..nobj];
    // Objects first (their own RELATIVE fixups), then the main program's PLT.
    for i in 1..nobj {
        if let Err(e) = relocate(loaded, i) {
            die_load(e);
        }
    }
    if let Err(e) = relocate(loaded, 0) {
        die_load(e);
    }

    // A thread-local has to be reachable before any initialiser runs, because an
    // initialiser may touch one; the block is installed once, here, for this
    // thread.
    if let Err(e) = unsafe { install_tls(loaded) } {
        die_load(e);
    }

    // Initialisers last: every object is relocated by now, which is what a
    // constructor calling into another object needs. The list is reverse
    // topological (see `load_dependencies`), so walking it backwards runs a
    // dependency's constructor before the constructor that may call into it. The
    // main program's own initialisers are run by its `crt0`, not here.
    let ctx = InitCtx { argc, argv, envp };
    for o in loaded[1..].iter().rev() {
        unsafe { run_initialisers(o, &ctx) };
    }

    entry
}
