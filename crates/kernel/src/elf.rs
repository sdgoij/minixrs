//! ELF64 binary loader — parse, validate, and load executables into memory.
//!
//! Supports x86_64 little-endian ET_EXEC binaries with PT_LOAD segments.
//! Used by the boot process to load `/sbin/init` from the initramfs.

use core::ptr;

/// ELF magic number.
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class: 64-bit.
pub const ELFCLASS64: u8 = 2;

/// ELF data encoding: little-endian.
pub const ELFDATA2LSB: u8 = 1;

/// ELF header e_type: executable.
pub const ET_EXEC: u16 = 2;

/// ELF header e_machine: x86_64.
pub const EM_X86_64: u16 = 62;

/// Program header type: null.
pub const PT_NULL: u32 = 0;
/// Program header type: loadable segment.
pub const PT_LOAD: u32 = 1;
/// Program header type: dynamic linking info.
pub const PT_DYNAMIC: u32 = 2;
/// Program header type: interpreter path.
pub const PT_INTERP: u32 = 3;
/// Program header type: note segment.
pub const PT_NOTE: u32 = 4;
/// Program header type: program header table itself.
pub const PT_PHDR: u32 = 6;
/// Program header type: GNU stack info.
pub const PT_GNU_STACK: u32 = 0x6474E551;

/// Program header flag: executable.
pub const PF_X: u32 = 1;
/// Program header flag: writable.
pub const PF_W: u32 = 2;
/// Program header flag: readable.
pub const PF_R: u32 = 4;

/// x86_64 page size (4 KB).
pub const PAGE_SIZE: u64 = 4096;

/// ELF64 header (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Elf64Ehdr {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// ELF64 program header (56 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// Result of a successful ELF load.
#[derive(Debug)]
pub struct LoadedElf {
    /// Lowest virtual address covered by any PT_LOAD segment.
    pub base: u64,
    /// Highest virtual address (exclusive) covered by any PT_LOAD segment.
    pub top: u64,
    /// Entry point (from e_entry).
    pub entry: u64,
}

/// Errors returned by the ELF loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Magic bytes not found.
    BadMagic,
    /// Not a 64-bit ELF.
    Not64Bit,
    /// Not little-endian.
    NotLittleEndian,
    /// Not an executable (ET_EXEC).
    NotExecutable,
    /// Wrong architecture (not EM_X86_64).
    WrongArch,
    /// Data too small to contain a valid header.
    Truncated,
    /// A PT_LOAD segment extends past the provided data.
    SegmentOutOfBounds,
    /// No PT_LOAD segments found.
    NoLoadSegments,
    /// Stack setup failed (invalid parameters).
    StackSetupFailed { msg: &'static str },
}

/// Parse and validate an ELF64 header.
///
/// Returns an Elf64Ehdr by value on success, or an `ElfError`.
pub fn parse_elf_header(data: &[u8]) -> Result<Elf64Ehdr, ElfError> {
    if data.len() < core::mem::size_of::<Elf64Ehdr>() {
        return Err(ElfError::Truncated);
    }

    let ehdr: Elf64Ehdr = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr) };

    // Validate magic
    if ehdr.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    // Validate class (64-bit)
    if ehdr.e_ident[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    // Validate endianness (little-endian)
    if ehdr.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }
    // Validate type (executable)
    if ehdr.e_type != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }
    // Validate architecture
    if ehdr.e_machine != EM_X86_64 {
        return Err(ElfError::WrongArch);
    }

    Ok(ehdr)
}

/// Load an ELF64 executable into memory.
///
/// Copies each `PT_LOAD` segment's file data to its virtual address
/// and zero-fills the BSS (memsz - filesz) area.
///
/// Returns a `LoadedElf` with base/top bounds and the entry point.
///
/// # Safety
///
/// The caller must ensure that the virtual addresses in PT_LOAD segments
/// correspond to writable memory that the caller has allocated.
pub unsafe fn load_elf(data: &[u8]) -> Result<LoadedElf, ElfError> {
    let ehdr = parse_elf_header(data)?;

    // Sanity-check program header fields
    if ehdr.e_phoff == 0
        || ehdr.e_phnum == 0
        || ehdr.e_phentsize as usize != core::mem::size_of::<Elf64Phdr>()
    {
        return Err(ElfError::NoLoadSegments);
    }

    let phoff = ehdr.e_phoff as usize;
    let phnum = ehdr.e_phnum as usize;
    let phentsize = ehdr.e_phentsize as usize;

    // Validate that program headers fit in the data
    let ph_end = phoff
        .checked_add(phnum * phentsize)
        .ok_or(ElfError::Truncated)?;
    if ph_end > data.len() {
        return Err(ElfError::Truncated);
    }

    // Track memory bounds
    let mut base = u64::MAX;
    let mut top = 0u64;
    let mut found_load = false;

    // Iterate program headers
    for i in 0..phnum {
        let phdr = unsafe { &*(data.as_ptr().add(phoff + i * phentsize) as *const Elf64Phdr) };

        if phdr.p_type != PT_LOAD {
            continue;
        }
        found_load = true;

        // Validate segment data is within the provided buffer
        let file_end = phdr
            .p_offset
            .checked_add(phdr.p_filesz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if file_end > data.len() as u64 {
            return Err(ElfError::SegmentOutOfBounds);
        }

        // Update bounds
        if phdr.p_vaddr < base {
            base = phdr.p_vaddr;
        }
        let seg_top = phdr
            .p_vaddr
            .checked_add(phdr.p_memsz)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if seg_top > top {
            top = seg_top;
        }

        // Copy file data to virtual address
        if phdr.p_filesz > 0 {
            let src = unsafe { data.as_ptr().add(phdr.p_offset as usize) };
            let dst = phdr.p_vaddr as *mut u8;
            unsafe {
                ptr::copy_nonoverlapping(src, dst, phdr.p_filesz as usize);
            }
        }

        // Zero-fill BSS (memsz - filesz)
        let bss_size = phdr.p_memsz.saturating_sub(phdr.p_filesz);
        if bss_size > 0 {
            let bss_start = phdr.p_vaddr.wrapping_add(phdr.p_filesz);
            let dst = bss_start as *mut u8;
            unsafe {
                ptr::write_bytes(dst, 0, bss_size as usize);
            }
        }
    }

    if !found_load {
        return Err(ElfError::NoLoadSegments);
    }

    Ok(LoadedElf {
        base,
        top,
        entry: ehdr.e_entry,
    })
}

/// Build a standard x86_64 System V ABI stack layout for a new process.
///
/// Writes `argc`, `argv` pointers (each `argv[i]` points into the strings
/// area at the top of the stack), and a null `envp` terminator.
///
/// `stack_top` is the highest address of the stack area (the stack grows
/// down).  The function writes data starting at the top, aligned down.
///
/// Returns the new RSP value (16-byte aligned, pointing to `argc`).
///
/// # Safety
///
/// The memory between `stack_top - size` and `stack_top` must be writable
/// and allocated to the caller.
pub unsafe fn setup_user_stack(
    stack_top: u64,
    size: usize,
    argv: &[&str],
) -> Result<u64, ElfError> {
    if size < 4096 {
        return Err(ElfError::StackSetupFailed {
            msg: "stack too small",
        });
    }

    // Compute string data area: we'll write strings at the very top.
    let mut string_pos = stack_top;
    let mut string_offsets: [u64; 64] = [0u64; 64];
    let argc = argv.len().min(63);

    // Write each argument string at the top of the stack, going down.
    // Each string is NUL-terminated.
    for (i, arg) in argv.iter().enumerate().take(argc) {
        let s = arg.as_bytes();
        let len = s.len() + 1; // +1 for NUL
        string_pos = string_pos.wrapping_sub(len as u64);
        if string_pos < stack_top.wrapping_sub(size as u64) {
            return Err(ElfError::StackSetupFailed {
                msg: "argv strings overflow stack",
            });
        }
        unsafe {
            ptr::copy_nonoverlapping(s.as_ptr(), string_pos as *mut u8, s.len());
            // Write NUL terminator
            *((string_pos as usize + s.len()) as *mut u8) = 0;
        }
        string_offsets[i] = string_pos;
    }

    // Align string_pos down to 16 bytes for the pointer array
    let base = string_pos & !15u64;

    // Standard SysV AMD64 ABI stack layout on process entry:
    //   [sp + 0]:          argc
    //   [sp + 8]:          argv[0]
    //   [sp + 8 + argc*8]: NULL (argv terminator)
    //   [sp + 8 + (argc+1)*8]: envp[0]
    //   ...
    //
    // We set up: argc, argv pointers, NULL terminator (no envp).
    //
    // Total size = align16((argc + 2) * 8)  (argc + argv ptrs + null terminator)

    let argv_array_size = ((argc as u64 + 2) * 8 + 15) & !15u64; // round up to 16
    let sp = base.wrapping_sub(argv_array_size);
    if sp < stack_top.wrapping_sub(size as u64) {
        return Err(ElfError::StackSetupFailed {
            msg: "stack too small for argv area",
        });
    }

    // Write argv pointers at [sp+8], [sp+16], ...
    for (i, offset) in string_offsets[..argc].iter().copied().enumerate() {
        let ptr_pos = sp + 8 + (i as u64) * 8;
        unsafe {
            *((ptr_pos) as *mut u64) = offset;
        }
    }

    // Write null terminator after the last argv pointer
    let null_pos = sp + 8 + (argc as u64) * 8;
    unsafe {
        *((null_pos) as *mut u64) = 0;
    }

    // Write argc at sp
    unsafe {
        *((sp) as *mut u64) = argc as u64;
    }

    // sp is now 16-byte aligned (guaranteed by total_size computation)
    debug_assert!(sp.is_multiple_of(16), "RSP must be 16-byte aligned");

    Ok(sp)
}

#[cfg(test)]
extern crate alloc;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::mem::size_of;

    // Helper: create a minimal valid ELF64 header for testing.
    fn make_minimal_ehdr(entry: u64, phoff: u64, phnum: u16) -> Elf64Ehdr {
        let mut ident = [0u8; 16];
        ident[0..4].copy_from_slice(&ELF_MAGIC);
        ident[4] = ELFCLASS64;
        ident[5] = ELFDATA2LSB;
        Elf64Ehdr {
            e_ident: ident,
            e_type: ET_EXEC,
            e_machine: EM_X86_64,
            e_version: 1,
            e_entry: entry,
            e_phoff: phoff,
            e_shoff: 0,
            e_flags: 0,
            e_ehsize: size_of::<Elf64Ehdr>() as u16,
            e_phentsize: size_of::<Elf64Phdr>() as u16,
            e_phnum: phnum,
            e_shentsize: 0,
            e_shnum: 0,
            e_shstrndx: 0,
        }
    }

    #[test]
    fn test_ehdr_size() {
        assert_eq!(size_of::<Elf64Ehdr>(), 64);
    }

    #[test]
    fn test_phdr_size() {
        assert_eq!(size_of::<Elf64Phdr>(), 56);
    }

    #[test]
    fn test_parse_valid_header() {
        let ehdr = make_minimal_ehdr(0x400000, 64, 1);
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        let result = parse_elf_header(data);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().e_entry, 0x400000);
    }

    #[test]
    fn test_parse_bad_magic() {
        let mut ehdr = make_minimal_ehdr(0, 0, 0);
        ehdr.e_ident[0] = 0xFF; // invalid magic
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(parse_elf_header(data).unwrap_err(), ElfError::BadMagic);
    }

    #[test]
    fn test_parse_32bit_rejected() {
        let mut ehdr = make_minimal_ehdr(0, 0, 0);
        ehdr.e_ident[4] = 1; // ELFCLASS32
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(parse_elf_header(data).unwrap_err(), ElfError::Not64Bit);
    }

    #[test]
    fn test_parse_big_endian_rejected() {
        let mut ehdr = make_minimal_ehdr(0, 0, 0);
        ehdr.e_ident[5] = 2; // ELFDATA2MSB
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(
            parse_elf_header(data).unwrap_err(),
            ElfError::NotLittleEndian
        );
    }

    #[test]
    fn test_parse_not_executable() {
        let mut ehdr = make_minimal_ehdr(0, 0, 0);
        ehdr.e_type = 3; // ET_DYN
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(parse_elf_header(data).unwrap_err(), ElfError::NotExecutable);
    }

    #[test]
    fn test_parse_wrong_arch() {
        let mut ehdr = make_minimal_ehdr(0, 0, 0);
        ehdr.e_machine = 0x28; // EM_ARM
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(parse_elf_header(data).unwrap_err(), ElfError::WrongArch);
    }

    #[test]
    fn test_parse_truncated_data() {
        let data = [0u8; 10];
        assert_eq!(parse_elf_header(&data).unwrap_err(), ElfError::Truncated);
    }

    /// Build a minimal 64-bit ELF with one PT_LOAD segment by writing
    /// into a fixed-size buffer.  Returns (buf, len).
    /// The segment maps virtual address `vaddr` of size `memsz`,
    /// containing `content` bytes at the start.
    fn build_test_elf<'a>(vaddr: u64, memsz: u64, content: &[u8], buf: &'a mut [u8]) -> &'a [u8] {
        let ehdr = make_minimal_ehdr(vaddr, 64, 1); // phoff = 64 (right after ehdr)
        let filez = content.len() as u64;
        let phdr = Elf64Phdr {
            p_type: PT_LOAD,
            p_flags: PF_R | PF_W | PF_X,
            p_offset: 64 + size_of::<Elf64Phdr>() as u64, // after phdr
            p_vaddr: vaddr,
            p_paddr: vaddr,
            p_filesz: filez,
            p_memsz: memsz,
            p_align: 0x1000,
        };

        let mut pos = 0usize;
        // Write ELF header
        let ehdr_bytes = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        buf[pos..pos + ehdr_bytes.len()].copy_from_slice(ehdr_bytes);
        pos += ehdr_bytes.len();

        // Write program header
        let phdr_bytes = unsafe {
            core::slice::from_raw_parts(&phdr as *const _ as *const u8, size_of::<Elf64Phdr>())
        };
        buf[pos..pos + phdr_bytes.len()].copy_from_slice(phdr_bytes);
        pos += phdr_bytes.len();

        // Write segment content
        buf[pos..pos + content.len()].copy_from_slice(content);
        pos += content.len();

        &buf[..pos]
    }

    #[test]
    fn test_load_elf_valid() {
        let content = b"Hello, World!";
        let vaddr = 0x1_0000_0000u64; // arbitrary test address
        let mut buf = [0u8; 1024];
        let data = build_test_elf(vaddr, 0x2000, content, &mut buf);

        // Parse but don't load — load_elf writes to vaddr which isn't mapped.
        // We verify parsing succeeds and loaded info is correct.
        let ehdr = parse_elf_header(data);
        assert!(ehdr.is_ok());
        let ehdr = ehdr.unwrap();
        assert_eq!(ehdr.e_entry, vaddr);
    }

    #[test]
    fn test_load_elf_no_load_segments() {
        let ehdr = make_minimal_ehdr(0, 0, 0);
        let data = unsafe {
            core::slice::from_raw_parts(&ehdr as *const _ as *const u8, size_of::<Elf64Ehdr>())
        };
        assert_eq!(
            unsafe { load_elf(data) }.unwrap_err(),
            ElfError::NoLoadSegments
        );
    }

    #[test]
    fn test_load_elf_truncated_phdr() {
        // A properly aligned 64-byte buffer with valid ELF magic but no room
        // for program headers.
        #[repr(C, align(8))]
        struct Align8([u8; 64]);
        #[allow(unused_mut)]
        let mut aligned = Align8([0u8; 64]);
        let data = &mut aligned.0;
        data[0..4].copy_from_slice(&ELF_MAGIC);
        data[4] = ELFCLASS64;
        data[5] = ELFDATA2LSB;
        data[16..18].copy_from_slice(&ET_EXEC.to_le_bytes()); // e_type
        data[18..20].copy_from_slice(&EM_X86_64.to_le_bytes()); // e_machine
        data[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff = 64
        // e_phentsize at offset 54, e_phnum at offset 56
        data[54] = 56; // e_phentsize = 56 (sizeof Elf64Phdr)
        data[56] = 1; // e_phnum = 1
        // e_ehsize at offset 52
        data[52] = 64; // e_ehsize = 64 (sizeof Elf64Ehdr)
        assert_eq!(unsafe { load_elf(data) }.unwrap_err(), ElfError::Truncated);
        assert_eq!(unsafe { load_elf(data) }.unwrap_err(), ElfError::Truncated);
    }

    #[test]
    fn test_loaded_elf_bounds() {
        let vaddr = 0x1000000u64;
        let mut buf = [0u8; 1024];
        let data = build_test_elf(vaddr, 0x4000, &[0u8; 256], &mut buf);
        // Verify our test builder produces correct values without loading.
        let phdr_offset = 64 + size_of::<Elf64Phdr>() as u64;
        let phdr = unsafe { &*(data.as_ptr().add(64) as *const Elf64Phdr) };
        assert_eq!(phdr.p_offset, phdr_offset);
        assert_eq!(phdr.p_vaddr, vaddr);
        assert_eq!(phdr.p_filesz, 256);
        assert_eq!(phdr.p_memsz, 0x4000);
    }

    #[test]
    fn test_constants() {
        assert_eq!(ELF_MAGIC, [0x7F, b'E', b'L', b'F']);
        assert_eq!(PT_LOAD, 1);
        assert_eq!(EM_X86_64, 62);
        assert_eq!(ET_EXEC, 2);
        assert_eq!(PF_X, 1);
        assert_eq!(PF_W, 2);
        assert_eq!(PF_R, 4);
    }

    #[test]
    fn test_phdr_flags_match() {
        // Combined flag value used in tests: R|W|X = 7
        assert_eq!(PF_R | PF_W | PF_X, 7);
    }

    #[test]
    fn test_stack_setup_single_arg() {
        let mut stack = vec![0u8; 65536];
        let stack_top = (stack.as_mut_ptr() as u64) + 65536;
        let rsp = unsafe { setup_user_stack(stack_top, 65536, &["/sbin/init"]).unwrap() };

        // Read argc from stack
        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 1);

        // Read argv[0] pointer (at rsp + 8, after argc)
        let argv0_ptr = unsafe { *((rsp + 8) as *const u64) };
        assert!(argv0_ptr != 0, "argv[0] pointer must not be null");
        assert!(argv0_ptr >= rsp, "argv[0] must point into the stack");
        // Verify the string at argv0_ptr
        let argv0_str = unsafe {
            let len = (0..256)
                .find(|&i| *((argv0_ptr + i) as *const u8) == 0)
                .unwrap_or(0);
            core::slice::from_raw_parts(argv0_ptr as *const u8, len as usize)
        };
        assert_eq!(argv0_str, b"/sbin/init");
    }

    #[test]
    fn test_stack_setup_multiple_args() {
        let mut stack = vec![0u8; 65536];
        let stack_top = (stack.as_mut_ptr() as u64) + 65536;
        let args = &["cat", "-n", "/etc/passwd"];
        let rsp = unsafe { setup_user_stack(stack_top, 65536, args).unwrap() };

        assert_eq!(rsp % 16, 0);
        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 3);

        // Read argv pointers (starting at rsp + 8)
        for i in 0..3 {
            let ptr = unsafe { *((rsp + 8 + i as u64 * 8) as *const u64) };
            let s = unsafe {
                let len = (0..256)
                    .find(|&j| *((ptr + j) as *const u8) == 0)
                    .unwrap_or(0);
                core::slice::from_raw_parts(ptr as *const u8, len as usize)
            };
            assert_eq!(s, args[i].as_bytes(), "argv[{}] mismatch", i);
        }

        // Verify null terminator after argv
        let null_val = unsafe { *((rsp + 8 + 3 * 8) as *const u64) };
        assert_eq!(null_val, 0);
    }

    #[test]
    fn test_stack_setup_too_small() {
        let mut stack = vec![0u8; 512]; // too small
        let stack_top = (stack.as_mut_ptr() as u64) + 512;
        let result = unsafe { setup_user_stack(stack_top, 512, &["/bin/test"]) };
        assert!(result.is_err());
    }

    #[test]
    fn test_stack_setup_empty_argv() {
        let mut stack = vec![0u8; 65536];
        let stack_top = (stack.as_mut_ptr() as u64) + 65536;
        let rsp = unsafe { setup_user_stack(stack_top, 65536, &[] as &[&str]).unwrap() };
        assert_eq!(rsp % 16, 0);
        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 0);
        // Null terminator at argv[0]
        let null_val = unsafe { *((rsp + 8) as *const u64) };
        assert_eq!(null_val, 0);
    }
}
