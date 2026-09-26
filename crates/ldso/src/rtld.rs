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
    DT_PLTRELSZ, DT_RELA, DT_RELASZ, DT_STRTAB, DT_SYMTAB, DYN_SIZE, ET_DYN, Elf, MAX_DYN, PF_W,
    PF_X, PT_LOAD, RELA_SIZE, Rela, SYM_SIZE, Sym,
};
use crate::layout::{
    BaseAlloc, TP_IS_PAST_THE_BLOCK, image_extent, page_down, page_up, tls_block_size,
};
use crate::reloc::{
    Def, RelocError, RelocImage, SYMNAME_MAX, Scope, SymError, SymName, TLS_MODULE_ID, apply_table,
    lookup_pass,
};
use crate::search::{PATH_MAX, SEARCH_PATH, names_a_path, search_path_join};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

/// What a header page that is not this loader's machine is reported as.
///
/// Per target rather than generic, because the interesting case is a loader and a
/// program built for different machines: naming the one this loader is for is
/// what makes that diagnosable.
#[cfg(target_arch = "x86_64")]
const NOT_ELF: &[u8] = b"ld.so: the image is not an x86_64 ELF\n";
#[cfg(target_arch = "riscv64")]
const NOT_ELF: &[u8] = b"ld.so: the image is not a riscv64 ELF\n";
#[cfg(target_arch = "aarch64")]
const NOT_ELF: &[u8] = b"ld.so: the image is not an aarch64 ELF\n";

const PAGE: u64 = 0x1000;
const MAX_OBJECTS: usize = 24;

/// The object table and the name buffer beside it are both locals in `load_graph`, so
/// they come out of the loader's stack — the 1 MiB every process's is
/// (`arch_*/hal.rs::user_stack_size`). Measured at `MAX_OBJECTS = 24`: 9048 bytes, or
/// under 1% of it. The pin is here because the reason to raise `MAX_OBJECTS` is the
/// *region* budget, which argues for a larger table every time it moves, and a loader
/// that runs off its stack dies somewhere that names nothing.
const _: () = assert!(
    core::mem::size_of::<[Object; MAX_OBJECTS]>() + core::mem::size_of::<[LibName; MAX_OBJECTS]>()
        <= 64 * 1024
);

fn write_bytes(b: &[u8]) {
    unsafe { minix_rt::write(2, b.as_ptr(), b.len()) };
}

fn die(msg: &[u8]) -> ! {
    write_bytes(msg);
    minix_rt::exit(1)
}

/// Why a load failed. Every one is fatal: a half-relocated program must not run.
enum LoadError {
    NotElf,
    NotObject,
    NoDynamic,
    TwoTlsModules,
    /// A `dlopen`'d object brings `PT_TLS`. The one thread-local module the loader places
    /// is laid out at startup, before any thread exists; a second would leave every
    /// thread's block sized for one image (`DYNAMIC_LINKING.md` D8).
    RuntimeTls,
    NoTlsSpace,
    TooManyObjects,
    CannotLoad,
    /// A `DT_NEEDED` was opened but would not stat, so there is no way to tell whether
    /// it is a file already loaded. Refused rather than mapped blind: a second mapping
    /// of one file is two copies of its data and a second run of its initialisers.
    CannotStat,
    /// VM refused a segment's `mmap`. `EAGAIN` is the address space having no room for
    /// another region, which is about the *process* rather than about the object; the
    /// rest travel as the errno they are, because the loader cannot name them all.
    Refused(i32),
    NoSpace,
    DtRel,
    Reloc(RelocError),
}

/// Longest message [`set_error`] writes, its NUL included: the longest real one is
/// `"ld.so: unresolved symbol "` + [`SYMNAME_MAX`] + `'\n'`.
const ERROR_MAX: usize = SYMNAME_MAX + 64;

/// Where a load failure's message waits, for the console or for `dlerror`.
///
/// `len` counts the message *without* the NUL [`set_error`] appends, and 0 means there is
/// nothing to report — which is what `dlerror` returns null for.
struct ErrorBuf {
    buf: [u8; ERROR_MAX],
    len: usize,
}

/// [`ErrorBuf`] behind the `UnsafeCell` a `static` needs.
struct ErrorCell(UnsafeCell<ErrorBuf>);

// SAFETY: the loader is one thread's program - see the note on `State`.
unsafe impl Sync for ErrorCell {}

static ERROR: ErrorCell = ErrorCell(UnsafeCell::new(ErrorBuf {
    buf: [0; ERROR_MAX],
    len: 0,
}));

/// Writes a message into a fixed buffer, stopping at its end.
struct Msg<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Msg<'_> {
    fn put(&mut self, b: &[u8]) {
        let n = (self.buf.len() - 1).saturating_sub(self.len).min(b.len());
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
    }

    fn dec(&mut self, mut v: u64) {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&tmp[i..]);
    }

    fn hex(&mut self, mut v: u64) {
        let mut tmp = [0u8; 16];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b"0123456789abcdef"[(v & 0xf) as usize];
            v >>= 4;
            if v == 0 {
                break;
            }
        }
        self.put(&tmp[i..]);
    }
}

/// The message a failure is reported with, in the buffer `dlerror` returns.
///
/// One formatter for both destinations: [`die_load`] writes what this returns to the
/// console and stops, and `dlopen` leaves it for `dlerror` to hand back. The wording of a
/// load failure therefore lives in one place whatever the caller did with it.
fn set_error(e: &LoadError) -> &'static [u8] {
    let cell = unsafe { &mut *ERROR.0.get() };
    let mut w = Msg {
        buf: &mut cell.buf,
        len: 0,
    };
    match e {
        LoadError::NotElf => w.put(NOT_ELF),
        LoadError::NotObject => w.put(b"ld.so: a DT_NEEDED is not a PIC object\n"),
        LoadError::NoDynamic => w.put(b"ld.so: no PT_DYNAMIC\n"),
        LoadError::TwoTlsModules => {
            w.put(b"ld.so: more than one loaded object has thread-local storage\n")
        }
        LoadError::RuntimeTls => {
            w.put(b"ld.so: a dlopen'd object has thread-local storage, and the loader");
            w.put(b" places one module\n");
        }
        LoadError::NoTlsSpace => w.put(b"ld.so: no room for a thread-local block\n"),
        LoadError::TooManyObjects => w.put(b"ld.so: too many objects\n"),
        LoadError::CannotLoad => w.put(b"ld.so: cannot load a shared object\n"),
        LoadError::CannotStat => w.put(b"ld.so: cannot stat a shared object\n"),
        LoadError::Refused(e) if *e == minix_std::EAGAIN => {
            w.put(b"ld.so: no room in the address space for another object\n")
        }
        LoadError::Refused(e) => {
            w.put(b"ld.so: VM refused a segment mapping (error ");
            w.dec(e.unsigned_abs() as u64);
            w.put(b")\n");
        }
        LoadError::NoSpace => w.put(b"ld.so: no room for another object\n"),
        LoadError::DtRel => w.put(b"ld.so: PLT relocations are not RELA\n"),
        LoadError::Reloc(RelocError::BadSize(_)) => w.put(b"ld.so: malformed RELA table size\n"),
        LoadError::Reloc(RelocError::BadTable(..)) => {
            w.put(b"ld.so: a relocation table is not in the image\n")
        }
        LoadError::Reloc(RelocError::Unsupported(t)) => {
            w.put(b"ld.so: unsupported relocation type ");
            w.dec(*t as u64);
            w.put(b"\n");
        }
        LoadError::Reloc(RelocError::BadSymbol(i, SymError::Missing)) => {
            w.put(b"ld.so: no name for symbol ");
            w.dec(*i as u64);
            w.put(b"\n");
        }
        LoadError::Reloc(RelocError::BadSymbol(i, SymError::TooLong)) => {
            w.put(b"ld.so: symbol ");
            w.dec(*i as u64);
            w.put(b" has too long a name\n");
        }
        LoadError::Reloc(RelocError::Unresolved(n)) => {
            w.put(b"ld.so: unresolved symbol ");
            w.put(n.as_bytes());
            w.put(b"\n");
        }
        LoadError::Reloc(RelocError::OutOfRange(a)) => {
            w.put(b"ld.so: relocation target outside the image: 0x");
            w.hex(*a);
            w.put(b"\n");
        }
        LoadError::Reloc(RelocError::TlsDescForASymbol) => {
            w.put(b"ld.so: a TLS descriptor names a symbol another object defines\n")
        }
    }
    cell.len = w.len;
    cell.buf[w.len] = 0;
    unsafe { core::slice::from_raw_parts(cell.buf.as_ptr(), cell.len) }
}

/// The message [`set_error`] stored, if any, and clears it: `dlerror`'s contract is to
/// report each failure once.
fn take_error() -> Option<&'static [u8]> {
    let cell = unsafe { &mut *ERROR.0.get() };
    if cell.len == 0 {
        return None;
    }
    // The message and its NUL, which is what the C caller is handed.
    let n = cell.len + 1;
    cell.len = 0;
    Some(unsafe { core::slice::from_raw_parts(cell.buf.as_ptr(), n) })
}

/// Forget a failure: a `dlopen` or `dlsym` that worked leaves `dlerror` with nothing to
/// report.
fn clear_error() {
    unsafe { (*ERROR.0.get()).len = 0 };
}

/// Store `text` as the pending failure. For the failures that are not a load's: a `dlsym`
/// of a name nothing defines, and a handle that names no object.
fn set_message(text: &[u8]) -> &'static [u8] {
    let cell = unsafe { &mut *ERROR.0.get() };
    let mut w = Msg {
        buf: &mut cell.buf,
        len: 0,
    };
    w.put(text);
    cell.len = w.len;
    cell.buf[w.len] = 0;
    unsafe { core::slice::from_raw_parts(cell.buf.as_ptr(), cell.len) }
}

fn die_load(e: LoadError) -> ! {
    die(set_error(&e))
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

/// The path a loaded object came from, so a second request for it does not map it
/// again. Bytes, because the string it came from lived in a file the loader has since
/// closed.
///
/// The *resolved* path — what [`load`] opened, not the `DT_NEEDED` spelling it came
/// from — because two spellings of one file resolve to one path, and that is the part
/// of "already loaded" that needs no syscall to see.
#[derive(Clone, Copy)]
struct LibName {
    buf: [u8; PATH_MAX],
    len: u8,
}

impl LibName {
    const EMPTY: Self = Self {
        buf: [0; PATH_MAX],
        len: 0,
    };

    /// Copy `name` in. `false` when it does not fit — callers have already refused
    /// names that long, so this cannot happen for a name that reached a load.
    fn set(&mut self, name: &[u8]) -> bool {
        if name.len() > PATH_MAX {
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

/// What identifies the file an object was mapped from: the device and inode `fstat`
/// reports for the descriptor it was mapped from.
///
/// Two spellings of one file — a path and a soname, a `..` in a path, a hard link —
/// share these, and no comparison of the names can see that they are one file. The
/// reference keeps both fields on every `Obj_Entry` and settles a second load with
/// them (`libexec/ld.elf_so/load.c:_rtld_load_object`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
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
    /// The path it was loaded from, empty for the main program.
    name: LibName,
    /// The file it was mapped from, `None` for the main program: the loader is given
    /// its header page, not a descriptor, so there is nothing to stat.
    identity: Option<Identity>,
    /// Whether its symbols are in the global scope, where a later object's relocations and
    /// a `RTLD_DEFAULT` lookup find them (what `dlopen`'s `RTLD_GLOBAL` asks for).
    /// Everything mapped before the program runs is global; `RTLD_LOCAL` is what leaves a
    /// `dlopen`'d object out of it, and then only [`group`](Self::group) sees it.
    global: bool,
    /// Which `dlopen` call loaded it, or 0 for the startup graph. A lookup tries the
    /// object's own group before the global scope, so an object's own definition of a name
    /// wins over a global one, and a `RTLD_LOCAL` object is reachable by the objects loaded
    /// with it.
    group: u16,
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
        identity: None,
        global: false,
        group: 0,
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
///
/// A lookup is two passes, load order within each: the objects in `owner`'s own `dlopen`
/// group first, then the global scope. That is what makes an object's own definition of a
/// name win over a global one of the same name, and it is why a `RTLD_LOCAL` object's
/// symbols are reachable by the objects loaded with it and by nothing else.
unsafe fn find_symbol(objects: &[Object], name: &[u8], scope: Scope, owner: usize) -> Option<Def> {
    // The loader is linkable to the objects it maps, the way a system `ld.so` is:
    // the names below belong to the loader and are in no object's tables.
    if let Some(def) = loader_defined(objects, owner, name) {
        return Some(def);
    }
    let group = objects.get(owner).map_or(0, |o| o.group);
    for pass in 0..2u8 {
        for (i, o) in objects.iter().enumerate() {
            if scope == Scope::ExcludeSelf && i == owner {
                continue;
            }
            if lookup_pass(o.group, o.global, group) != Some(pass) {
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
                    if value < o.img_start || value.checked_add(size).is_none_or(|e| e > o.img_end)
                    {
                        return None;
                    }
                    return Some(Def { addr: value, size });
                }
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
    // The `dlopen` family, for the same reason: nothing a `cdylib` links against defines
    // them, so the loader answers — and the loader is the object that can load one.
    // `minix-libc`'s shared build calls through these names
    // (`crates/minix-libc/src/lib.rs`), which is how a C program gets `dlopen` without the
    // loader exporting a `.dynsym` of its own: the loader is static, and a program's
    // relocations resolve against it by name here rather than against a link map.
    for (n, f) in [
        (&b"__rtld_dlopen"[..], dlopen as *const ()),
        (&b"__rtld_dlsym"[..], dlsym as *const ()),
        (&b"__rtld_dlerror"[..], dlerror as *const ()),
        (&b"__rtld_dlclose"[..], dlclose as *const ()),
    ] {
        if name == n {
            return Some(Def {
                addr: f as u64,
                size: 0,
            });
        }
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

    fn tlsdesc_static(&self) -> u64 {
        __tlsdesc_static as *const () as u64
    }
}

/// Map one shared object's segments at `base` and read its dynamic tables.
///
/// `path` and `identity` are what the object is and where it came from, filled in
/// here because they are what a second request for the same file is matched against
/// and there is no moment between this call and the list at which the object exists
/// without them.
fn read_and_map(
    fd: i32,
    path: &[u8],
    identity: Identity,
    alloc: &mut BaseAlloc,
) -> Result<Object, LoadError> {
    if path.len() > PATH_MAX {
        return Err(LoadError::CannotLoad);
    }
    let mut hdr = [0u8; PAGE as usize];
    let n = minix_rt::read(fd, &mut hdr);
    if n < 64 {
        return Err(LoadError::NotObject);
    }
    let elf = Elf::new(&hdr[..n as usize]).map_err(|_| LoadError::NotObject)?;
    if elf.e_machine() != crate::reloc::TARGET.e_machine || elf.e_type() != ET_DYN {
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
        // The page this segment starts on can also be an earlier segment's, and
        // then the union of the two is what that page must be mapped with — see
        // `layout::shared_page_flags`.
        let flags = crate::layout::shared_page_flags(&elf, i).ok_or(LoadError::NotObject)?;
        if flags & PF_W != 0 {
            prot |= minix_rt::vmem::PROT_WRITE;
        }
        if flags & PF_X != 0 {
            prot |= minix_rt::vmem::PROT_EXEC;
        }
        let r = unsafe {
            minix_rt::vmem::mmap_status(
                va as *mut u8,
                len as usize,
                prot,
                minix_rt::vmem::MAP_PRIVATE | minix_rt::vmem::MAP_FIXED,
                fd,
                off as i64,
            )
        };
        // VM's own reason, when it has one: a full region table is not a property of
        // the image, and `MAP_FAILED` alone used to make the two look alike.
        let r = r.map_err(LoadError::Refused)?;
        if r as u64 != va {
            return Err(LoadError::NotObject);
        }
        // A `PT_LOAD`'s memory size can exceed its file size: the tail is the
        // segment's zero-initialised data (`.bss`). `mmap` fills a page from the
        // file as far as the *file* goes, and the bytes that follow a segment's
        // data in a `.so` are not padding — LLD puts the symbol table, the string
        // table and the section headers at the end — so the tail has to be cleared
        // here. The exec path gets the same result from the in-file end VFS sends
        // (`VM_VFS_MMAP` in `vm/mod.rs`), which VM zero-fills past; a plain `mmap`
        // only has POSIX's "past the end of the file", which this tail is not.
        //
        // The pages are faulted in and made private by the write, which is what
        // the exec path does for the image's data regions too. A tail in a segment
        // that is not writable cannot be cleared from here; the linker does not
        // make one, because `.bss` is writable by definition.
        if p.p_memsz > p.p_filesz && p.p_flags & PF_W != 0 {
            let dst = (base + p.p_vaddr + p.p_filesz) as *mut u8;
            unsafe { ptr::write_bytes(dst, 0, (p.p_memsz - p.p_filesz) as usize) };
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
        identity: Some(identity),
        // Both of these are the caller's to set: the startup walk makes every object it
        // adds global (`load_dependencies`), and a `dlopen` gives the object the scope of
        // the call that made it (`load_now`).
        global: false,
        group: 0,
    };
    if !obj.name.set(path) {
        return Err(LoadError::CannotLoad);
    }
    obj.symtab = unsafe { obj.dyn_tag(DT_SYMTAB) }.ok_or(LoadError::NoDynamic)?;
    obj.strtab = unsafe { obj.dyn_tag(DT_STRTAB) }.ok_or(LoadError::NoDynamic)?;
    obj.nsym = unsafe { obj.sym_count() };
    Ok(obj)
}

/// One candidate path, and what is behind it.
enum Candidate {
    /// A file none of `objects` was mapped from: the object, mapped and described.
    Fresh(Object),
    /// A file an object in the list already maps, under this path or another spelling
    /// of it — the index of that object. The descriptor is closed and nothing is mapped.
    ///
    /// The index is what `dlopen` needs: a second request for a file that is already
    /// loaded hands back that object's handle rather than a second mapping.
    Loaded(usize),
    /// Nothing is at that path, so the next search-path directory may still have the
    /// name.
    Absent,
}

/// Open `path` and decide what it is: a file to map, one already mapped, or none.
///
/// Both of the loader's "already loaded" checks are here, and in the reference's
/// order. The path costs nothing — an object that came from this exact path *is* this
/// object — so it comes first. What the open adds is the other spelling of it: only
/// `st_dev`/`st_ino` can say that a path and a soname, a `..`, or a hard link are one
/// file, which is why the reference opens and stats *before* it decides
/// (`libexec/ld.elf_so/load.c:_rtld_load_object`).
fn open_candidate(
    path: &[u8],
    objects: &[Object],
    alloc: &mut BaseAlloc,
) -> Result<Candidate, LoadError> {
    if objects.iter().any(|o| o.name.as_bytes() == path) {
        let i = objects
            .iter()
            .position(|o| o.name.as_bytes() == path)
            .ok_or(LoadError::CannotLoad)?;
        return Ok(Candidate::Loaded(i));
    }
    let fd = minix_rt::open(path, 0);
    if fd < 0 {
        return Ok(Candidate::Absent);
    }
    let fd = fd as i32;
    let Ok(st) = minix_std::fs::fstat(fd) else {
        minix_rt::close(fd);
        return Err(LoadError::CannotStat);
    };
    let identity = Identity {
        dev: st.st_dev,
        ino: st.st_ino,
    };
    if let Some(i) = objects.iter().position(|o| o.identity == Some(identity)) {
        minix_rt::close(fd);
        return Ok(Candidate::Loaded(i));
    }
    let obj = read_and_map(fd, path, identity, alloc);
    minix_rt::close(fd);
    Ok(Candidate::Fresh(obj?))
}

/// The object `name` names, resolved the way a `DT_NEEDED` is: a path is the file, a bare
/// name is searched for, and either may turn out to be a file already mapped.
///
/// One place decides that for both loads — the startup walk and a `dlopen` — because the
/// rules are the same and the second caller needs the *index* of an object that is already
/// there, which is what [`Candidate::Loaded`] carries.
fn find_candidate(
    name: &[u8],
    objects: &[Object],
    alloc: &mut BaseAlloc,
) -> Result<Candidate, LoadError> {
    if names_a_path(name) {
        // A hard-coded pathname is the file, not a name to look for: the search path
        // is for bare names only (`search.c`).
        if name.len() > PATH_MAX {
            return Err(LoadError::CannotLoad);
        }
        return open_candidate(name, objects, alloc);
    }
    let mut path = [0u8; PATH_MAX];
    for dir in SEARCH_PATH {
        let n = search_path_join(dir, name, &mut path).ok_or(LoadError::CannotLoad)?;
        match open_candidate(&path[..n], objects, alloc)? {
            Candidate::Absent => continue,
            // The first directory that has the file decides: a later one would name the
            // same file, or nothing at all.
            found => return Ok(found),
        }
    }
    Ok(Candidate::Absent)
}

/// Open a `DT_NEEDED` name and map the object behind it, unless one is loaded.
///
/// `Ok(None)` is that case: the name resolved to a file the list already has, whether
/// by the path it resolved to or by the file itself. `objects` must be the objects
/// loaded so far — the whole point is to not add a second mapping of one of them.
fn load(
    name: &[u8],
    objects: &[Object],
    alloc: &mut BaseAlloc,
) -> Result<Option<Object>, LoadError> {
    match find_candidate(name, objects, alloc)? {
        Candidate::Fresh(obj) => Ok(Some(obj)),
        Candidate::Loaded(_) => Ok(None),
        Candidate::Absent => Err(LoadError::CannotLoad),
    }
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
unsafe fn needed_names(obj: &Object, out: &mut [LibName; MAX_OBJECTS]) -> Result<usize, LoadError> {
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
            } else {
                // Dropping it would be a load that half-happened, and silently: the
                // object it names would simply never be there, with nothing said about
                // why. There is no such thing as a partial load — the main program's
                // relocations against it would fail, or worse, resolve elsewhere.
                return Err(LoadError::CannotLoad);
            }
        }
        p += DYN_SIZE as u64;
    }
    Ok(n)
}

/// Load `owner`'s `DT_NEEDED` objects depth-first, appending each one *before*
/// following its own dependencies. That makes the list a reverse topological
/// order — every object appears before the objects it needs — and it is what
/// makes a library graph load once: a name already in the list is skipped, so a
/// diamond (two objects naming one library) or a cycle terminates instead of
/// filling the list. Relocations do not depend on the order, because a lookup
/// searches every object; the initialisers do, and walk the list backwards.
///
/// "Already in the list" is [`load`]'s decision, and it is about the file rather than
/// about the `DT_NEEDED` spelling: one library named by two objects, or by one object
/// twice under a name and a path, is one mapping.
unsafe fn load_dependencies(
    objects: &mut [Object; MAX_OBJECTS],
    count: &mut usize,
    alloc: &mut BaseAlloc,
    owner: usize,
    scope: (bool, u16),
) -> Result<(), LoadError> {
    let mut names = [LibName::EMPTY; MAX_OBJECTS];
    let n = unsafe { needed_names(&objects[owner], &mut names) }?;
    for name in names.iter().take(n) {
        if *count == MAX_OBJECTS {
            return Err(LoadError::TooManyObjects);
        }
        let obj = match load(name.as_bytes(), &objects[..*count], alloc)? {
            Some(o) => o,
            None => continue,
        };
        let idx = *count;
        objects[idx] = obj;
        // Everything the startup graph holds is in the global scope: it is what a
        // `RTLD_LOCAL` object is defined *against*, and the second pass of a lookup. A
        // `dlopen`'s walk passes its own scope down instead, so a driver's dependencies
        // land in that call's group.
        objects[idx].global = scope.0;
        objects[idx].group = scope.1;
        *count += 1;
        unsafe { load_dependencies(objects, count, alloc, idx, scope) }?;
    }
    Ok(())
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

/// A `TLSDESC` descriptor: what a thread-local access in that dialect reads and
/// calls. AArch64's codegen emits it by default (the linker decides, per
/// relocation, whether the pair can be resolved statically), and the loader is
/// what fills both words — [`__tlsdesc_static`] and the variable's offset from the
/// thread pointer.
#[repr(C)]
struct TlsDesc {
    resolver: u64,
    arg: u64,
}

/// The calling thread's thread pointer.
///
/// Each target keeps it somewhere else, and only x86_64's needs a trick: there is
/// no unprivileged way to read the FS base, but the port's own convention supplies
/// one — the runtime's `tls_block_alloc` writes the pointer at `[tp]`, and
/// [`install_tls`] does the same, so that word *is* the pointer. aarch64 reads
/// `TPIDR_EL0`, which the kernel reloads on every context switch; riscv64 reads
/// `tp`, which the runtime settles itself once the syscall has told the kernel.
///
/// Reading it per call, rather than caching it, is what lets a thread
/// `pthread_create` started work: its block is its own, and only the distance from
/// the pointer to the storage is fixed.
#[cfg(target_arch = "x86_64")]
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

#[cfg(target_arch = "aarch64")]
fn read_tp() -> u64 {
    let tp: u64;
    unsafe {
        asm!(
            "mrs {tp}, tpidr_el0",
            tp = out(reg) tp,
            options(nostack, nomem, preserves_flags),
        )
    };
    tp
}

#[cfg(target_arch = "riscv64")]
fn read_tp() -> u64 {
    let tp: u64;
    unsafe { asm!("mv {tp}, tp", tp = out(reg) tp, options(nostack, nomem)) };
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
    // The storage starts at the thread pointer itself on aarch64/riscv64, and one
    // block-length below it on x86_64. See `layout::TP_IS_PAST_THE_BLOCK`.
    let base = if TP_IS_PAST_THE_BLOCK {
        read_tp().wrapping_sub(size)
    } else {
        read_tp()
    };
    base.wrapping_add(ti.offset) as *mut u8
}

/// The `TLSDESC` resolver a descriptor this load wrote points at: the "static"
/// one, whose whole answer is its argument — the variable's offset from the thread
/// pointer, which the linker resolved into the relocation's addend because the
/// variable is the one object's own. The storage is the same single block
/// [`install_tls`] placed, so an access through this and one through
/// `__tls_get_addr` reach the same address.
///
/// The dialect's other resolver — the one that looks a module up, for a symbol
/// another object defines — is not reachable: the port places one object's
/// thread-local storage, and the walk refuses a descriptor that names a symbol
/// before this can be called with one.
#[unsafe(no_mangle)]
unsafe extern "C" fn __tlsdesc_static(desc: *const TlsDesc) -> usize {
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return 0;
    };
    desc.arg as usize
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
    }
    TLS_BLOCK_SIZE.store(size, Ordering::Relaxed);
    minix_rt::thread_set_tls(thread_pointer_for(block, size) as usize);
    Ok(())
}

/// The thread pointer for a block of `size` bytes at `block`.
///
/// x86_64's pointer sits past the block, 16-aligned, with the self-pointer
/// [`read_tp`] reads the FS base back through. aarch64 and riscv64 point at the
/// block's first byte, so there is nothing to read back and nothing to round.
fn thread_pointer_for(block: u64, size: u64) -> u64 {
    if TP_IS_PAST_THE_BLOCK {
        let tp = (block + size + 15) & !15;
        unsafe {
            ptr::write(tp as *mut u64, tp);
        }
        tp
    } else {
        block
    }
}

/// Load every object the main program needs, relocate everything, run every
// ---- dlopen ----

/// `dlopen`'s `RTLD_GLOBAL`: the object's symbols join the global scope. Without it a load
/// is `RTLD_LOCAL`, and only the objects loaded with it and its own handle see them.
///
/// `RTLD_LAZY` and `RTLD_NOW` (`tools/c-include/dlfcn.h`) are not read here, because this
/// loader binds eagerly either way (D6): resolving everything up front is a superset of
/// what `RTLD_LAZY` promises, so the two ask for the same thing of it.
const RTLD_GLOBAL: u32 = 0x0100;

/// The loader's memory of what it loaded.
///
/// [`run`] returns to the main program, and a `dlopen` from that program arrives here
/// afterwards, in this same image — so nothing a load needs may be a local of `run`. The
/// object list, its length, the base allocator and the program's arguments live here
/// instead.
///
/// Not synchronised: two threads in `dlopen` at once would race on the table. The rest of
/// the loader is the same — `install_tls` writes its statics once, before any object runs —
/// and the consumer `dlopen` exists for, loading a driver once at startup, is one call on
/// one thread. A lock belongs here the day something needs one.
struct State {
    objects: [Object; MAX_OBJECTS],
    count: usize,
    alloc: BaseAlloc,
    /// The program's own arguments, for the initialisers a `dlopen` runs: an ELF ABI
    /// constructor is called with the three the program's entry point gets.
    argc: u64,
    argv: u64,
    envp: u64,
    /// The next `dlopen` group id. 0 is the startup graph; 1 is the first `dlopen`.
    next_group: u16,
}

/// [`State`] behind the `UnsafeCell` a `static` needs.
struct StateCell(UnsafeCell<State>);

// SAFETY: see the note on [`State`].
unsafe impl Sync for StateCell {}

static STATE: StateCell = StateCell(UnsafeCell::new(State {
    objects: [Object::EMPTY; MAX_OBJECTS],
    count: 0,
    alloc: BaseAlloc::new(),
    argc: 0,
    argv: 0,
    envp: 0,
    next_group: 1,
}));

/// The loader's state. Every caller is the loader's own code.
unsafe fn state() -> &'static mut State {
    unsafe { &mut *STATE.0.get() }
}

/// The NUL-terminated string at `p`, as bytes, or `None` when it runs past what a path may
/// be. Bounded rather than trusting the caller: this reads a string from the program.
unsafe fn cstr(p: *const u8) -> Option<&'static [u8]> {
    if p.is_null() {
        return None;
    }
    for i in 0..=PATH_MAX {
        if unsafe { *p.add(i) } == 0 {
            return Some(unsafe { core::slice::from_raw_parts(p, i) });
        }
    }
    None
}

/// The index a `dlopen` handle names: the address of the `Object` it was made from.
unsafe fn handle_index(handle: *mut u8) -> Option<usize> {
    let st = unsafe { state() };
    let h = handle as usize;
    (0..st.count).find(|&i| ptr::addr_of!(st.objects[i]) as usize == h)
}

/// Load `name` now, at run time, and add it to the table: what the startup walk does to an
/// object, for one object, when the program asks.
///
/// Returns its index, which is also its handle. `DT_NEEDED` names are followed the same
/// way, so an object that names a library the program does not have brings it; the whole
/// group is relocated (dependencies first) and every initialiser in it runs, so the object
/// is complete when this returns — which is what eager binding means (D6). Nothing about a
/// load is incremental.
///
/// `global` is `RTLD_GLOBAL`. On failure the group is out of the table, so nothing looks it
/// up afterwards, but its mappings stay: this port has no `munmap` for the loader to give
/// them back with, and a program that tries one driver, fails, and tries the next relies on
/// the *failure* being recoverable rather than on the memory being recovered.
unsafe fn load_now(name: &[u8], global: bool) -> Result<usize, LoadError> {
    let st = unsafe { state() };
    let first = match find_candidate(name, &st.objects[..st.count], &mut st.alloc)? {
        Candidate::Loaded(i) => return Ok(i),
        Candidate::Absent => return Err(LoadError::CannotLoad),
        Candidate::Fresh(obj) => {
            if obj.tls.is_some() {
                return Err(LoadError::RuntimeTls);
            }
            if st.count == MAX_OBJECTS {
                return Err(LoadError::TooManyObjects);
            }
            let i = st.count;
            st.objects[i] = obj;
            st.objects[i].global = global;
            st.objects[i].group = st.next_group;
            st.next_group = st.next_group.wrapping_add(1);
            st.count = i + 1;
            i
        }
    };

    // The object's own `DT_NEEDED`, in the same group, before anything is relocated.
    let group = st.objects[first].group;
    if let Err(e) = unsafe {
        load_dependencies(
            &mut st.objects,
            &mut st.count,
            &mut st.alloc,
            first,
            (global, group),
        )
    } {
        st.count = first;
        return Err(e);
    }
    // A dependency may be the one that brought thread-local storage, and the loader places
    // one module for the whole process (`install_tls`): refused before anything runs.
    if st.objects[first..st.count].iter().any(|o| o.tls.is_some()) {
        st.count = first;
        return Err(LoadError::RuntimeTls);
    }

    // Dependencies first: a `COPY` reads the source object's bytes, and the source has to
    // have been relocated before there are any. Then the object itself.
    for i in (first..st.count).rev() {
        if let Err(e) = relocate(&st.objects[..st.count], i) {
            st.count = first;
            return Err(e);
        }
    }

    // The group's initialisers, dependencies first: `load_dependencies` appended each
    // object before the objects it needs, so walking the group backwards is the order a
    // constructor that may call into a dependency requires.
    let ctx = InitCtx {
        argc: st.argc,
        argv: st.argv,
        envp: st.envp,
    };
    for i in (first..st.count).rev() {
        unsafe { run_initialisers(&st.objects[i], &ctx) };
    }
    Ok(first)
}

/// `dlopen(3)`: map a shared object now, and return a handle for `dlsym`.
///
/// `NULL` for `path` is the main program — the global scope. That is what a caller asking
/// for its own symbols, or for `dlsym(RTLD_DEFAULT, …)`, means, and it is a handle that is
/// not null, so success is distinguishable from failure.
///
/// `RTLD_LAZY` and `RTLD_NOW` are both accepted and mean the same thing: this loader binds
/// eagerly (D6), which is a superset of what `RTLD_LAZY` promises. `RTLD_GLOBAL` adds the
/// object's symbols to the global scope; without it they belong to the object's group.
///
/// Null on failure, with the reason left for `dlerror`.
unsafe extern "C" fn dlopen(path: *const u8, flags: i32) -> *mut u8 {
    let global = flags as u32 & RTLD_GLOBAL != 0;
    if path.is_null() {
        let st = unsafe { state() };
        clear_error();
        return if st.count == 0 {
            ptr::null_mut()
        } else {
            (&mut st.objects[0] as *mut Object).cast::<u8>()
        };
    }
    let name = match unsafe { cstr(path) } {
        Some(n) if !n.is_empty() => n,
        _ => {
            set_error(&LoadError::CannotLoad);
            return ptr::null_mut();
        }
    };
    match unsafe { load_now(name, global) } {
        Ok(i) => {
            clear_error();
            let st = unsafe { state() };
            (&mut st.objects[i] as *mut Object).cast::<u8>()
        }
        Err(e) => {
            set_error(&e);
            ptr::null_mut()
        }
    }
}

/// `dlsym(3)`: the address `name` has in the scope `handle` names, or null.
///
/// A null handle is `RTLD_DEFAULT`, the global scope. `RTLD_NEXT` — the next definition
/// after the caller's — is refused rather than answered wrongly: finding the caller's
/// object means reading a return address, and nothing needs that yet.
unsafe extern "C" fn dlsym(handle: *mut u8, name: *const u8) -> *mut u8 {
    if handle as usize == usize::MAX {
        set_message(
            b"ld.so: RTLD_NEXT needs the caller's return address, which nothing here reads\n",
        );
        return ptr::null_mut();
    }
    let Some(name) = (unsafe { cstr(name) }) else {
        set_message(b"ld.so: dlsym was given no name\n");
        return ptr::null_mut();
    };
    let st = unsafe { state() };
    let owner = if handle.is_null() {
        0
    } else {
        match unsafe { handle_index(handle) } {
            Some(i) => i,
            None => {
                set_message(b"ld.so: dlsym was given a handle no loaded object has\n");
                return ptr::null_mut();
            }
        }
    };
    match unsafe { find_symbol(&st.objects[..st.count], name, Scope::All, owner) } {
        Some(d) => {
            clear_error();
            d.addr as *mut u8
        }
        None => {
            set_message(b"ld.so: no such symbol\n");
            ptr::null_mut()
        }
    }
}

/// `dlclose(3)`: accepted, and nothing is unloaded.
///
/// The object stays mapped and in the table — its regions, its relocations and its
/// initialisers are all still there — so a second `dlopen` of the same file hands back the
/// same handle and a caller that wanted the memory back has no way to ask for it here.
/// That is what a caller that checks the return value is told: nothing failed.
unsafe extern "C" fn dlclose(handle: *mut u8) -> i32 {
    if handle.is_null() || unsafe { handle_index(handle) }.is_some() {
        0
    } else {
        set_message(b"ld.so: dlclose was given a handle no loaded object has\n");
        -1
    }
}

/// `dlerror(3)`: the last failure's message, once, or null when there is none.
///
/// The message is the loader's own — the same one a failed load would have printed and
/// stopped on — because both come from [`set_error`].
unsafe extern "C" fn dlerror() -> *mut u8 {
    match take_error() {
        Some(t) => t.as_ptr() as *mut u8,
        None => ptr::null_mut(),
    }
}

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
    if elf.e_machine() != crate::reloc::TARGET.e_machine {
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
        // The loader has the main program's header page, not a descriptor for it, so there
        // is nothing to stat. It needs no identity: a `DT_NEEDED` cannot name
        // it, and nothing is ever compared against it — the empty name above is not a
        // path either.
        identity: None,
        // The main program is the first thing in the global scope, and every lookup's
        // second pass can reach it.
        global: true,
        group: 0,
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

    // The table is a `static`, not a local: the loader keeps running after it hands the
    // main program its entry point, and the `dlopen` that program may make later arrives
    // in this image (`State`).
    let st = unsafe { state() };
    st.objects[0] = main;
    st.count = 1;
    st.alloc = BaseAlloc::new();
    st.next_group = 1;
    // A `dlopen`'d object's initialisers are called with the program's own arguments, so
    // these outlive this call too.
    st.argc = argc;
    st.argv = argv;
    st.envp = envp;
    if let Err(e) =
        unsafe { load_dependencies(&mut st.objects, &mut st.count, &mut st.alloc, 0, (true, 0)) }
    {
        die_load(e);
    }

    let loaded = &st.objects[..st.count];
    // Objects first (their own RELATIVE fixups), then the main program's PLT.
    for i in 1..st.count {
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
