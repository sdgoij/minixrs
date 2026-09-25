//! Phase 0 runtime linking.
//!
//! The loader is entered as a program's `PT_INTERP` interpreter. The kernel has
//! mapped the main program's ELF header page read-only and put its address in
//! the register `_start` reads (`r9`); from there the loader takes `e_entry` and
//! the program headers, maps each `DT_NEEDED` object, resolves the symbols those
//! objects (and the main program) need, patches the main program's PLT/GOT, and
//! returns `e_entry` for its caller to enter.
//!
//! Classic non-PIE: the main program's addresses are absolute, and the objects
//! are `ET_DYN` mapped at [`DSO_BASE`]. Eager binding only — no resolve
//! trampoline, so an unresolved symbol is a load failure rather than a
//! first-call fault.

use crate::elf::{
    DT_HASH, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTREL, DT_PLTRELSZ, DT_RELA, DT_RELASZ, DT_STRTAB,
    DT_SYMTAB, DYN_SIZE, EM_X86_64, Elf, MAX_DYN, PF_W, PF_X, PT_LOAD, RELA_SIZE, SYM_SIZE,
};
use core::ptr;

/// Where `ET_DYN` objects are mapped. Fixed (no ASLR), and clear of the main
/// program (`0x0100_0000`), the loader itself (`0x0400_0000`), the stack
/// (`0x0FE0_0000`) and the heap (`0x3FE0_0000`).
pub const DSO_BASE: u64 = 0x0200_0000;

/// A `DT_NEEDED` name is looked up here, in order.
const SEARCH_PATH: &[&[u8]] = &[b"/lib/", b"/usr/lib/"];

const PAGE: u64 = 0x1000;
const MAX_NEEDED: usize = 8;
const MAX_OBJECTS: usize = 8;
const MAX_SYMS: usize = 256;

const ELF64_MAGIC: [u8; 4] = *b"\x7fELF";

fn page_down(x: u64) -> u64 {
    x & !(PAGE - 1)
}

fn page_up(x: u64) -> u64 {
    (x + PAGE - 1) & !(PAGE - 1)
}

fn die(msg: &[u8]) -> ! {
    unsafe { minix_rt::write(2, msg.as_ptr(), msg.len()) };
    minix_rt::exit(1)
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

unsafe fn cstr_va(va: u64) -> &'static [u8] {
    unsafe {
        let mut n = 0usize;
        while ptr::read((va + n as u64) as *const u8) != 0 {
            n += 1;
        }
        core::slice::from_raw_parts(va as *const u8, n)
    }
}

/// Value of the first `tag` in the dynamic array at `dyn_va` (a runtime VA).
unsafe fn dyn_tag(dyn_va: u64, tag: i64) -> Option<u64> {
    let mut p = dyn_va;
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

/// The dynamic symbol count, from the `.hash` table's `nchain` (ELF requires
/// `DT_HASH` alongside `DT_GNU_HASH` for compatibility, and LLD emits both).
unsafe fn sym_count(dyn_va: u64, bias: u64) -> usize {
    match unsafe { dyn_tag(dyn_va, DT_HASH) } {
        Some(h) => unsafe { rd_u32(bias + h + 4) as usize },
        None => 0,
    }
}

/// One loaded ELF object: where its dynamic array and symbol tables are, and the
/// bias its link-time addresses are relative to (`0` for a non-PIE executable).
#[derive(Clone, Copy)]
struct Object {
    dyn_va: u64,
    bias: u64,
    symtab: u64,
    strtab: u64,
    nsym: usize,
}

impl Object {
    const EMPTY: Object = Object {
        dyn_va: 0,
        bias: 0,
        symtab: 0,
        strtab: 0,
        nsym: 0,
    };

    /// The name of dynamic symbol `idx`, if it is a defined global or weak.
    unsafe fn defined_name(&self, idx: u32) -> Option<&'static [u8]> {
        if idx as usize >= self.nsym {
            return None;
        }
        let st = self.symtab + (idx as u64) * SYM_SIZE as u64;
        let st_name = unsafe { rd_u32(st) };
        let st_info = unsafe { ptr::read((st + 4) as *const u8) };
        let st_shndx = unsafe { ptr::read_unaligned((st + 6) as *const u16) };
        if st_name == 0 || st_shndx == 0 {
            return None;
        }
        let bind = st_info >> 4;
        if bind != 1 && bind != 2 {
            // STB_GLOBAL, STB_WEAK
            return None;
        }
        Some(unsafe { cstr_va(self.strtab + st_name as u64) })
    }

    /// The name of dynamic symbol `idx`, defined or not (for a relocation's
    /// symbol).
    unsafe fn name_of(&self, idx: u32) -> Option<&'static [u8]> {
        if idx == 0 || idx as usize >= self.nsym {
            return None;
        }
        let st = self.symtab + (idx as u64) * SYM_SIZE as u64;
        let st_name = unsafe { rd_u32(st) };
        if st_name == 0 {
            return None;
        }
        Some(unsafe { cstr_va(self.strtab + st_name as u64) })
    }

    /// The runtime address of this object's `idx`th symbol (for definitions).
    unsafe fn value_of(&self, idx: u32) -> u64 {
        let st = self.symtab + (idx as u64) * SYM_SIZE as u64;
        self.bias + unsafe { rd_u64(st + 8) }
    }
}

/// The address a symbol name resolves to, searched across every loaded object.
unsafe fn find_symbol(objects: &[Object], name: &[u8]) -> Option<u64> {
    for o in objects {
        for i in 0..o.nsym.min(MAX_SYMS) as u32 {
            if let Some(n) = unsafe { o.defined_name(i) }
                && n == name
            {
                return Some(unsafe { o.value_of(i) });
            }
        }
    }
    None
}

/// Apply a `RELA` range at runtime VA `rela_va` spanning `bytes`.
unsafe fn apply_rela(objects: &[Object], owner: &Object, rela_va: u64, bytes: usize) {
    let count = bytes / RELA_SIZE;
    for i in 0..count {
        let p = rela_va + (i as u64) * RELA_SIZE as u64;
        let r_offset = unsafe { rd_u64(p) };
        let r_info = unsafe { rd_u64(p + 8) };
        let r_addend = unsafe { rd_u64(p + 16) } as i64;
        let typ = (r_info & 0xffff_ffff) as u32;
        let sym = (r_info >> 32) as u32;
        let at = owner.bias + r_offset;
        match typ {
            8 => unsafe { wr_u64(at, owner.bias.wrapping_add(r_addend as u64)) }, // RELATIVE
            1 | 6 | 7 => {
                // R_X86_64_64, GLOB_DAT, JUMP_SLOT
                let name = unsafe { owner.name_of(sym) };
                match name.and_then(|n| unsafe { find_symbol(objects, n) }) {
                    Some(addr) => unsafe { wr_u64(at, addr.wrapping_add(r_addend as u64)) },
                    None => die(b"ld.so: unresolved symbol\n"),
                }
            }
            _ => die(b"ld.so: unsupported relocation\n"),
        }
    }
}

/// Relocate one object: its own `RELATIVE`/`GLOB_DAT` tables, then its PLT.
unsafe fn relocate(objects: &[Object], o: &Object) {
    if let Some(rela) = unsafe { dyn_tag(o.dyn_va, DT_RELA) } {
        let sz = unsafe { dyn_tag(o.dyn_va, DT_RELASZ).unwrap_or(0) };
        unsafe { apply_rela(objects, o, o.bias + rela, sz as usize) };
    }
    if let Some(jmprel) = unsafe { dyn_tag(o.dyn_va, DT_JMPREL) } {
        let sz = unsafe { dyn_tag(o.dyn_va, DT_PLTRELSZ).unwrap_or(0) };
        let kind = unsafe { dyn_tag(o.dyn_va, DT_PLTREL).unwrap_or(DT_RELA as u64) };
        if kind != DT_RELA as u64 {
            die(b"ld.so: DT_REL PLT (unexpected)\n");
        }
        unsafe { apply_rela(objects, o, o.bias + jmprel, sz as usize) };
    }
}

/// Map one shared object's segments at `base` and read its dynamic tables.
fn map_object(fd: i32, base: u64) -> Option<Object> {
    let mut hdr = [0u8; PAGE as usize];
    let n = minix_rt::read(fd, &mut hdr);
    if n < 64 {
        return None;
    }
    let elf = match Elf::new(&hdr[..n as usize]) {
        Ok(e) => e,
        Err(_) => return None,
    };
    if elf.e_machine() != EM_X86_64 {
        return None;
    }

    for i in 0..elf.e_phnum() as usize {
        let p = elf.phdr(i)?;
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
            return None;
        }
    }

    let dynp = elf.dynamic_phdr()?;
    let dyn_va = base + dynp.p_vaddr;
    let symtab = base + unsafe { dyn_tag(dyn_va, DT_SYMTAB)? };
    let strtab = base + unsafe { dyn_tag(dyn_va, DT_STRTAB)? };
    let nsym = unsafe { sym_count(dyn_va, base) };
    Some(Object {
        dyn_va,
        bias: base,
        symtab,
        strtab,
        nsym,
    })
}

/// Open a `DT_NEEDED` name from the search path and map it.
fn load(name: &[u8]) -> Option<Object> {
    if name.len() + 16 > 128 {
        return None;
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
        let obj = map_object(fd, DSO_BASE);
        minix_rt::close(fd);
        return obj;
    }
    None
}

/// Load every `DT_NEEDED` of `main`, relocate everything, and return the main
/// program's entry point.
///
/// # Safety
///
/// `main_hdr` must be the VA of the main program's ELF header page, mapped
/// read-only by VFS (the kernel passes it in the loader's entry register).
pub unsafe fn run(main_hdr: u64) -> u64 {
    let hdr = unsafe { core::slice::from_raw_parts(main_hdr as *const u8, PAGE as usize) };
    let elf = match Elf::new(hdr) {
        Ok(e) => e,
        Err(_) => die(b"ld.so: main header page is not ELF\n"),
    };
    if elf.bytes()[..4] != ELF64_MAGIC {
        die(b"ld.so: main magic\n");
    }
    let entry = elf.e_entry();
    let dynp = match elf.dynamic_phdr() {
        Some(p) => p,
        None => die(b"ld.so: main has no PT_DYNAMIC\n"),
    };

    // Non-PIE: the main's dynamic tables are at absolute addresses.
    let dyn_va = dynp.p_vaddr;
    let main = Object {
        dyn_va,
        bias: 0,
        symtab: unsafe { dyn_tag(dyn_va, DT_SYMTAB).unwrap_or(0) },
        strtab: unsafe { dyn_tag(dyn_va, DT_STRTAB).unwrap_or(0) },
        nsym: unsafe { sym_count(dyn_va, 0) },
    };

    let mut objects = [Object::EMPTY; MAX_OBJECTS];
    objects[0] = main;
    let mut nobj = 1usize;

    // Load each DT_NEEDED.
    let mut p = dyn_va;
    let mut needed = 0usize;
    for _ in 0..MAX_DYN {
        let t = unsafe { rd_u64(p) } as i64;
        if t == DT_NULL {
            break;
        }
        if t == DT_NEEDED {
            let name = unsafe { cstr_va(main.strtab + rd_u64(p + 8)) };
            if needed >= MAX_NEEDED || nobj >= MAX_OBJECTS {
                die(b"ld.so: too many objects\n");
            }
            needed += 1;
            match load(name) {
                Some(o) => {
                    objects[nobj] = o;
                    nobj += 1;
                }
                None => die(b"ld.so: cannot load DT_NEEDED\n"),
            }
        }
        p += DYN_SIZE as u64;
    }

    let loaded = &objects[..nobj];
    // Objects first (their own RELATIVE fixups), then the main's PLT.
    for o in &loaded[1..] {
        unsafe { relocate(loaded, o) };
    }
    unsafe { relocate(loaded, &main) };

    entry
}
