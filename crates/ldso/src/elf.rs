//! ELF64 parsing for the dynamic loader.
//!
//! Everything here reads the file bytes with explicit little-endian field reads
//! rather than `#[repr(C)]` casts: an interpreter maps a shared object at a page
//! boundary, so its dynamic/symbol/relocation tables land at arbitrary offsets
//! inside that mapping and a cast would be an unaligned reference. Link-time
//! virtual addresses are resolved to file offsets through the `PT_LOAD` table
//! ([`Elf::va_to_offset`]); that is how `DT_STRTAB`, `DT_SYMTAB` and the
//! relocation tables are read out of the mapped file.

pub const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;

pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;

pub const EM_X86_64: u16 = 62;
pub const EM_RISCV: u16 = 243;
pub const EM_AARCH64: u16 = 183;

pub const PT_NULL: u32 = 0;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_NOTE: u32 = 4;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_STACK: u32 = 0x6474_e551;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_INIT: i64 = 12;
pub const DT_SONAME: i64 = 14;
pub const DT_REL: i64 = 17;
pub const DT_RELSZ: i64 = 18;
pub const DT_RELENT: i64 = 19;
pub const DT_PLTREL: i64 = 20;
pub const DT_JMPREL: i64 = 23;
pub const DT_INIT_ARRAY: i64 = 25;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_GNU_HASH: i64 = 0x6fff_fef5;
pub const DT_RELACOUNT: i64 = 0x6fff_fff9;
pub const DT_RELCOUNT: i64 = 0x6fff_fffa;

pub const R_X86_64_NONE: u32 = 0;
pub const R_X86_64_64: u32 = 1;
pub const R_X86_64_COPY: u32 = 5;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE: u32 = 8;
/// Fills a `tls_index`'s `ti_module` with the module the store belongs to. The
/// general- (or local-) dynamic TLS model reaches its thread-locals through
/// `__tls_get_addr`, and this is the id it is handed.
pub const R_X86_64_DTPMOD64: u32 = 16;

pub const EHDR_SIZE: usize = 64;
pub const PHDR_SIZE: usize = 56;
pub const DYN_SIZE: usize = 16;
pub const SYM_SIZE: usize = 24;
pub const RELA_SIZE: usize = 24;
pub const REL_SIZE: usize = 16;

/// Bound on a dynamic array that carries no `DT_NULL` terminator.
pub const MAX_DYN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dyn {
    pub d_tag: i64,
    pub d_val: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

impl Sym {
    pub const SHN_UNDEF: u16 = 0;
    pub const SHN_ABS: u16 = 0xfff1;

    pub const STB_GLOBAL: u8 = 1;
    pub const STB_WEAK: u8 = 2;

    pub fn bind(&self) -> u8 {
        self.st_info >> 4
    }

    pub fn typ(&self) -> u8 {
        self.st_info & 0xf
    }

    pub fn is_undef(&self) -> bool {
        self.st_shndx == Self::SHN_UNDEF
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rela {
    pub r_offset: u64,
    pub r_info: u64,
    pub r_addend: i64,
}

impl Rela {
    pub fn sym(&self) -> u32 {
        (self.r_info >> 32) as u32
    }

    pub fn typ(&self) -> u32 {
        self.r_info as u32
    }
}

fn rd<const N: usize>(b: &[u8], o: usize) -> Option<[u8; N]> {
    b.get(o..o + N)?.try_into().ok()
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    rd::<2>(b, o).map(u16::from_le_bytes)
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    rd::<4>(b, o).map(u32::from_le_bytes)
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    rd::<8>(b, o).map(u64::from_le_bytes)
}

fn i64_at(b: &[u8], o: usize) -> Option<i64> {
    u64_at(b, o).map(|v| v as i64)
}

/// An ELF64 file image — the bytes of the mapped file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Elf<'a> {
    bytes: &'a [u8],
}

impl<'a> Elf<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < EHDR_SIZE {
            return Err(ElfError::TooShort);
        }
        if bytes[0..4] != ELF_MAGIC {
            return Err(ElfError::BadMagic);
        }
        if bytes[4] != ELFCLASS64 {
            return Err(ElfError::Not64Bit);
        }
        if bytes[5] != ELFDATA2LSB {
            return Err(ElfError::NotLittleEndian);
        }
        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn e_type(&self) -> u16 {
        u16_at(self.bytes, 16).unwrap_or(0)
    }

    pub fn e_machine(&self) -> u16 {
        u16_at(self.bytes, 18).unwrap_or(0)
    }

    pub fn e_entry(&self) -> u64 {
        u64_at(self.bytes, 24).unwrap_or(0)
    }

    pub fn e_phoff(&self) -> u64 {
        u64_at(self.bytes, 32).unwrap_or(0)
    }

    pub fn e_phentsize(&self) -> u16 {
        u16_at(self.bytes, 54).unwrap_or(0)
    }

    pub fn e_phnum(&self) -> u16 {
        u16_at(self.bytes, 56).unwrap_or(0)
    }

    pub fn phdr(&self, i: usize) -> Option<Phdr> {
        if i >= self.e_phnum() as usize {
            return None;
        }
        let es = self.e_phentsize() as usize;
        if es != PHDR_SIZE {
            return None;
        }
        let o = self.e_phoff() as usize + i * es;
        Some(Phdr {
            p_type: u32_at(self.bytes, o)?,
            p_flags: u32_at(self.bytes, o + 4)?,
            p_offset: u64_at(self.bytes, o + 8)?,
            p_vaddr: u64_at(self.bytes, o + 16)?,
            p_filesz: u64_at(self.bytes, o + 32)?,
            p_memsz: u64_at(self.bytes, o + 40)?,
            p_align: u64_at(self.bytes, o + 48)?,
        })
    }

    /// The first `PT_DYNAMIC` segment, if the file has one.
    pub fn dynamic_phdr(&self) -> Option<Phdr> {
        (0..self.e_phnum() as usize)
            .filter_map(|i| self.phdr(i))
            .find(|p| p.p_type == PT_DYNAMIC)
    }

    /// The image's `PT_TLS` segment, if it has one: the thread-local storage a
    /// shared object brings with it.
    ///
    /// The segment is a *template*: `p_vaddr .. p_vaddr + p_filesz` is the
    /// initialised image, which lies inside a `PT_LOAD` and is therefore mapped
    /// with the rest of the object, and `p_memsz` is the size every thread's
    /// block needs. The loader gives an object that has one a slot in a thread's
    /// block (`crates/ldso/src/rtld.rs`).
    pub fn tls_phdr(&self) -> Option<Phdr> {
        (0..self.e_phnum() as usize)
            .filter_map(|i| self.phdr(i))
            .find(|p| p.p_type == PT_TLS)
    }

    /// The file offset a link-time virtual address lives at, from the `PT_LOAD`
    /// table. `None` when no loadable segment covers it, or when it is at or
    /// past that segment's file size (bss).
    pub fn va_to_offset(&self, va: u64) -> Option<usize> {
        for i in 0..self.e_phnum() as usize {
            let p = self.phdr(i)?;
            if p.p_type != PT_LOAD || va < p.p_vaddr {
                continue;
            }
            let delta = va - p.p_vaddr;
            if delta < p.p_filesz {
                return Some((p.p_offset + delta) as usize);
            }
        }
        None
    }

    /// A NUL-terminated string in the file at link-time address `va`.
    pub fn cstr(&self, va: u64) -> Option<&'a [u8]> {
        let o = self.va_to_offset(va)?;
        let rest = self.bytes.get(o..)?;
        let end = rest.iter().position(|&c| c == 0)?;
        Some(&rest[..end])
    }

    pub fn dyn_at(&self, va: u64) -> Option<Dyn> {
        let o = self.va_to_offset(va)?;
        Some(Dyn {
            d_tag: i64_at(self.bytes, o)?,
            d_val: u64_at(self.bytes, o + 8)?,
        })
    }

    /// The `DT_NULL`-terminated dynamic array starting at link-time address `va`.
    pub fn dyn_entries(&self, va: u64) -> DynIter<'a> {
        DynIter {
            elf: *self,
            va,
            left: MAX_DYN,
        }
    }

    pub fn sym_at(&self, symtab_va: u64, index: u32) -> Option<Sym> {
        let o = self.va_to_offset(symtab_va)? + index as usize * SYM_SIZE;
        Some(Sym {
            st_name: u32_at(self.bytes, o)?,
            st_info: *self.bytes.get(o + 4)?,
            st_other: *self.bytes.get(o + 5)?,
            st_shndx: u16_at(self.bytes, o + 6)?,
            st_value: u64_at(self.bytes, o + 8)?,
            st_size: u64_at(self.bytes, o + 16)?,
        })
    }

    pub fn rela_at(&self, rela_va: u64, index: usize) -> Option<Rela> {
        let o = self.va_to_offset(rela_va)? + index * RELA_SIZE;
        Some(Rela {
            r_offset: u64_at(self.bytes, o)?,
            r_info: u64_at(self.bytes, o + 8)?,
            r_addend: i64_at(self.bytes, o + 16)?,
        })
    }
}

pub struct DynIter<'a> {
    elf: Elf<'a>,
    va: u64,
    left: usize,
}

impl Iterator for DynIter<'_> {
    type Item = Dyn;

    fn next(&mut self) -> Option<Dyn> {
        if self.va == 0 || self.left == 0 {
            return None;
        }
        self.left -= 1;
        let d = self.elf.dyn_at(self.va)?;
        if d.d_tag == DT_NULL {
            self.va = 0;
            return None;
        }
        self.va = self.va.checked_add(DYN_SIZE as u64)?;
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x200_0000;
    const PHOFF: usize = EHDR_SIZE;
    const STRTAB_OFF: usize = PHOFF + 2 * PHDR_SIZE; // 176
    const DYN_OFF: usize = 192;
    const FILE_LEN: usize = 256;
    const ENTRY: u64 = BASE + 0x1000;

    fn push16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push64(v: &mut Vec<u8>, x: u64) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    /// One image: a `PT_LOAD` covering the file, a `PT_DYNAMIC`, a strtab holding
    /// `libdyn.so` and a three-entry dynamic array naming it.
    fn build() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&ELF_MAGIC);
        b.push(ELFCLASS64);
        b.push(ELFDATA2LSB);
        b.push(1); // EI_VERSION
        b.extend_from_slice(&[0u8; 9]);
        push16(&mut b, ET_DYN);
        push16(&mut b, EM_X86_64);
        push32(&mut b, 1); // e_version
        push64(&mut b, ENTRY);
        push64(&mut b, PHOFF as u64);
        push64(&mut b, 0); // e_shoff
        push32(&mut b, 0); // e_flags
        push16(&mut b, EHDR_SIZE as u16);
        push16(&mut b, PHDR_SIZE as u16);
        push16(&mut b, 2); // e_phnum
        push16(&mut b, 0);
        push16(&mut b, 0);
        push16(&mut b, 0);
        assert_eq!(b.len(), EHDR_SIZE);

        // PT_LOAD: whole file, readable+writable at BASE.
        push32(&mut b, PT_LOAD);
        push32(&mut b, PF_R | PF_W);
        push64(&mut b, 0);
        push64(&mut b, BASE);
        push64(&mut b, BASE); // p_paddr
        push64(&mut b, FILE_LEN as u64);
        push64(&mut b, FILE_LEN as u64);
        push64(&mut b, 0x1000);
        // PT_DYNAMIC at DYN_OFF.
        push32(&mut b, PT_DYNAMIC);
        push32(&mut b, PF_R | PF_W);
        push64(&mut b, DYN_OFF as u64);
        push64(&mut b, BASE + DYN_OFF as u64);
        push64(&mut b, 0);
        push64(&mut b, 64);
        push64(&mut b, 64);
        push64(&mut b, 8);
        assert_eq!(b.len(), PHOFF + 2 * PHDR_SIZE);

        // strtab at STRTAB_OFF, then pad to DYN_OFF.
        b.extend_from_slice(b"libdyn.so\0");
        b.resize(DYN_OFF, 0);

        // dynamic array
        push64(&mut b, DT_NEEDED as u64);
        push64(&mut b, 0); // offset of "libdyn.so" in strtab
        push64(&mut b, DT_STRTAB as u64);
        push64(&mut b, BASE + STRTAB_OFF as u64);
        push64(&mut b, DT_STRSZ as u64);
        push64(&mut b, 10);
        push64(&mut b, DT_NULL as u64);
        push64(&mut b, 0);
        assert_eq!(b.len(), FILE_LEN);
        b
    }

    #[test]
    fn header_fields_parse() {
        let b = build();
        let e = Elf::new(&b).expect("valid");
        assert_eq!(e.e_type(), ET_DYN);
        assert_eq!(e.e_machine(), EM_X86_64);
        assert_eq!(e.e_entry(), ENTRY);
        assert_eq!(e.e_phoff(), PHOFF as u64);
        assert_eq!(e.e_phnum(), 2);
        assert_eq!(e.e_phentsize(), PHDR_SIZE as u16);
    }

    #[test]
    fn rejects_bad_magic_and_class() {
        let mut b = build();
        b[1] = b'X';
        assert_eq!(Elf::new(&b), Err(ElfError::BadMagic));
        let mut b = build();
        b[4] = 1;
        assert_eq!(Elf::new(&b), Err(ElfError::Not64Bit));
        assert_eq!(Elf::new(&[0u8; 4]), Err(ElfError::TooShort));
    }

    #[test]
    fn program_headers_parse_and_dynamic_is_found() {
        let b = build();
        let e = Elf::new(&b).unwrap();
        let load = e.phdr(0).unwrap();
        assert_eq!(load.p_type, PT_LOAD);
        assert_eq!(load.p_vaddr, BASE);
        assert_eq!(load.p_filesz, FILE_LEN as u64);
        let dynp = e.phdr(1).unwrap();
        assert_eq!(dynp.p_type, PT_DYNAMIC);
        assert_eq!(e.dynamic_phdr(), Some(dynp));
        assert_eq!(e.phdr(2), None);
    }

    #[test]
    fn tls_is_reported_from_the_program_headers() {
        let b = build();
        assert_eq!(
            Elf::new(&b).unwrap().tls_phdr(),
            None,
            "no PT_TLS in the image"
        );

        // The same image, with its first program header made a PT_TLS. What the
        // loader needs out of it is the segment itself: the initialised image and
        // the size every thread's copy needs are all in the header.
        let mut b = build();
        b[PHOFF..PHOFF + 4].copy_from_slice(&PT_TLS.to_le_bytes());
        let tls = Elf::new(&b).unwrap().tls_phdr().unwrap();
        assert_eq!(tls.p_type, PT_TLS);
        assert_eq!(tls.p_vaddr, BASE);
        assert_eq!(tls.p_filesz, FILE_LEN as u64);
    }

    #[test]
    fn va_to_offset_uses_load_segments() {
        let b = build();
        let e = Elf::new(&b).unwrap();
        assert_eq!(e.va_to_offset(BASE + STRTAB_OFF as u64), Some(STRTAB_OFF));
        assert_eq!(e.va_to_offset(BASE + DYN_OFF as u64), Some(DYN_OFF));
        // At/past p_filesz is bss, not a file offset.
        assert_eq!(e.va_to_offset(BASE + FILE_LEN as u64), None);
        assert_eq!(e.va_to_offset(BASE - 1), None);
    }

    #[test]
    fn cstr_reads_the_string_table() {
        let b = build();
        let e = Elf::new(&b).unwrap();
        assert_eq!(e.cstr(BASE + STRTAB_OFF as u64), Some(&b"libdyn.so"[..]));
    }

    #[test]
    fn dynamic_array_terminates_and_names_the_library() {
        let b = build();
        let e = Elf::new(&b).unwrap();
        let entries: Vec<Dyn> = e.dyn_entries(BASE + DYN_OFF as u64).collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            Dyn {
                d_tag: DT_NEEDED,
                d_val: 0
            }
        );
        assert_eq!(entries[1].d_tag, DT_STRTAB);
        // DT_NEEDED's value is an offset into DT_STRTAB's string table.
        assert_eq!(
            e.cstr(entries[1].d_val + entries[0].d_val),
            Some(&b"libdyn.so"[..])
        );
    }

    #[test]
    fn dyn_iter_ends_when_terminator_is_absent() {
        // A dynamic pointer into unmapped file offset yields no entries rather
        // than spinning: dyn_at returns None and the iterator stops.
        let b = build();
        let e = Elf::new(&b).unwrap();
        assert_eq!(e.dyn_entries(BASE + FILE_LEN as u64).count(), 0);
    }
}
