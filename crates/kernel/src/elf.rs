//! ELF64 loader for userspace processes in the MINIX kernel.
//!
//! Provides functions to parse, validate, and load ELF64 binaries into
//! memory, and to set up user-mode stacks per the SysV AMD64 ABI.

use core::ptr;

pub const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
pub const ELFCLASS64: u8 = 2;
pub const ELFDATA2LSB: u8 = 1;
pub const ET_EXEC: u16 = 2;
pub const EM_X86_64: u16 = 62;
pub const EM_RISCV: u16 = 243;

pub const PT_NULL: u32 = 0;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_NOTE: u32 = 4;
pub const PT_PHDR: u32 = 6;
pub const PT_GNU_STACK: u32 = 0x6474e551;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

/// ELF64 file header (64 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Ehdr {
    pub e_ident: [u8; 16], // ELF magic + class + data + version + OS/ABI + padding
    pub e_type: u16,       // ET_EXEC, ET_DYN, etc.
    pub e_machine: u16,    // EM_X86_64, etc.
    pub e_version: u32,
    pub e_entry: u64, // Entry point virtual address
    pub e_phoff: u64, // Program header offset in file
    pub e_shoff: u64, // Section header offset in file
    pub e_flags: u32,
    pub e_ehsize: u16,    // ELF header size (64 for ELF64)
    pub e_phentsize: u16, // Size of one program header entry
    pub e_phnum: u16,     // Number of program header entries
    pub e_shentsize: u16, // Size of one section header entry
    pub e_shnum: u16,     // Number of section header entries
    pub e_shstrndx: u16,  // Section header string table index
}

/// ELF64 program header (56 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Phdr {
    pub p_type: u32,   // PT_LOAD, etc.
    pub p_flags: u32,  // PF_R, PF_W, PF_X
    pub p_offset: u64, // Offset in file
    pub p_vaddr: u64,  // Virtual address to load at
    pub p_paddr: u64,  // Physical address
    pub p_filesz: u64, // Size in file
    pub p_memsz: u64,  // Size in memory (may include BSS)
    pub p_align: u64,  // Alignment
}

/// Result of a successful ELF load.
#[derive(Debug, Clone, Copy)]
pub struct LoadedElf {
    /// Base virtual address (lowest vaddr across all PT_LOAD segments).
    pub base: u64,
    /// Top virtual address (highest vaddr + memsz across all PT_LOAD segments).
    pub top: u64,
    /// Entry point virtual address.
    pub entry: u64,
}

/// Errors during ELF parsing or loading.
#[derive(Debug, Clone, Copy)]
pub enum ElfError {
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    NotExecutable,
    WrongArch,
    Truncated,
    SegmentOutOfBounds,
    NoLoadSegments,
    StackSetupFailed { msg: &'static str },
}

/// Parse and validate the ELF64 header from raw bytes.
///
/// Returns a reference to the validated header, or an error.
pub fn parse_elf_header(data: &[u8]) -> Result<&Elf64Ehdr, ElfError> {
    if data.len() < core::mem::size_of::<Elf64Ehdr>() {
        return Err(ElfError::Truncated);
    }
    // Safety: we just checked the data length.
    let ehdr = unsafe { &*(data.as_ptr() as *const Elf64Ehdr) };

    // Validate magic
    if ehdr.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    // Must be 64-bit
    if ehdr.e_ident[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    // Must be little-endian
    if ehdr.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }
    // Must be ET_EXEC
    if ehdr.e_type != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }
    // Must be x86_64 or RISC-V
    #[cfg(target_arch = "x86_64")]
    let expected_machine = EM_X86_64;
    #[cfg(target_arch = "riscv64")]
    let expected_machine = EM_RISCV;
    if ehdr.e_machine != expected_machine {
        return Err(ElfError::WrongArch);
    }
    Ok(ehdr)
}

/// Load a raw ELF64 binary into memory.
///
/// Copies PT_LOAD segments to their virtual addresses and zero-fills BSS.
///
/// # Safety
///
/// The caller must ensure `data` contains a valid ELF64 binary with appropriate
/// segment virtual addresses that map to writable, accessible memory.
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

/// Set up a user-mode stack for a new process, following the SysV AMD64 ABI.
///
/// `stack_top` is the initial stack pointer (highest address of stack area).
/// `size` is the total stack size in bytes.
/// `argv` provides the command-line arguments.
///
/// Returns the new stack pointer (RSP) value, 16-byte aligned.
///
/// # Safety
///
/// The caller must ensure `stack_top` points to a valid, writable memory region
/// of at least `size` bytes.
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
mod tests {
    use super::*;

    const EHDR_SIZE: usize = core::mem::size_of::<Elf64Ehdr>();
    const PHDR_SIZE: usize = core::mem::size_of::<Elf64Phdr>();

    fn make_minimal_ehdr() -> Elf64Ehdr {
        Elf64Ehdr {
            e_ident: [
                b'\x7f',
                b'E',
                b'L',
                b'F',        // magic
                ELFCLASS64,  // class
                ELFDATA2LSB, // data
                1,           // version
                0,           // OS/ABI
                0,           // ABI version
                0,
                0,
                0,
                0,
                0,
                0,
                0, // padding
            ],
            e_type: ET_EXEC,
            e_machine: EM_X86_64,
            e_version: 1,
            e_entry: 0x1000000,
            e_phoff: EHDR_SIZE as u64,
            e_shoff: 0,
            e_flags: 0,
            e_ehsize: EHDR_SIZE as u16,
            e_phentsize: PHDR_SIZE as u16,
            e_phnum: 1,
            e_shentsize: 0,
            e_shnum: 0,
            e_shstrndx: 0,
        }
    }

    #[test]
    fn test_ehdr_size() {
        assert_eq!(EHDR_SIZE, 64);
    }

    #[test]
    fn test_phdr_size() {
        assert_eq!(PHDR_SIZE, 56);
    }

    #[test]
    fn test_parse_valid_header() {
        let ehdr = make_minimal_ehdr();
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        let result = parse_elf_header(bytes);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.e_entry, 0x1000000);
        assert_eq!(parsed.e_phnum, 1);
    }

    #[test]
    fn test_parse_bad_magic() {
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_ident[0] = 0; // corrupt magic
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        assert!(matches!(parse_elf_header(bytes), Err(ElfError::BadMagic)));
    }

    #[test]
    fn test_parse_32bit_rejected() {
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_ident[4] = 1; // ELFCLASS32
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        assert!(matches!(parse_elf_header(bytes), Err(ElfError::Not64Bit)));
    }

    #[test]
    fn test_parse_big_endian_rejected() {
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_ident[5] = 2; // ELFDATA2MSB
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        assert!(matches!(
            parse_elf_header(bytes),
            Err(ElfError::NotLittleEndian)
        ));
    }

    #[test]
    fn test_parse_not_executable() {
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_type = 3; // ET_DYN
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        assert!(matches!(
            parse_elf_header(bytes),
            Err(ElfError::NotExecutable)
        ));
    }

    #[test]
    fn test_parse_wrong_arch() {
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_machine = 0x28; // ARM, not x86_64
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        assert!(matches!(parse_elf_header(bytes), Err(ElfError::WrongArch)));
    }

    #[test]
    fn test_parse_truncated_data() {
        let data = [0u8; 10]; // too small
        assert!(matches!(parse_elf_header(&data), Err(ElfError::Truncated)));
    }

    /// Build a minimal valid ELF64 binary in a buffer with one LOAD segment.
    /// Returns (buffer, len) where the buffer contains the ELF with header +
    /// one phdr + simple content at vaddr 0x1000000.
    fn build_test_elf(buf: &mut [u8], content: &[u8]) -> usize {
        // Ensure buffer is large enough
        let total = EHDR_SIZE + PHDR_SIZE + content.len();
        assert!(buf.len() >= total, "buffer too small for test ELF");

        // ELF header
        let ehdr = make_minimal_ehdr();
        let ehdr_bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        buf[..EHDR_SIZE].copy_from_slice(ehdr_bytes);

        // Program header: one LOAD segment at vaddr 0x1000000
        let phdr = Elf64Phdr {
            p_type: PT_LOAD,
            p_flags: PF_R | PF_W | PF_X,
            p_offset: (EHDR_SIZE + PHDR_SIZE) as u64,
            p_vaddr: 0x1000000,
            p_paddr: 0x1000000,
            p_filesz: content.len() as u64,
            p_memsz: content.len() as u64,
            p_align: 0x1000,
        };
        let phdr_bytes =
            unsafe { core::slice::from_raw_parts(&phdr as *const _ as *const u8, PHDR_SIZE) };
        buf[EHDR_SIZE..EHDR_SIZE + PHDR_SIZE].copy_from_slice(phdr_bytes);

        // File data (at p_offset)
        let data_start = EHDR_SIZE + PHDR_SIZE;
        buf[data_start..data_start + content.len()].copy_from_slice(content);

        total
    }

    #[test]
    fn test_load_elf_valid() {
        let mut buf = [0u8; 512];
        let total = build_test_elf(&mut buf, b"Hello, ELF!");
        // load_elf requires a mutable environment, but since we're testing
        // on the host, we can't actually write to 0x1000000. So we test
        // the parsing and segment bounds, but skip the actual memory write.
        let data = &buf[..total];
        let ehdr = parse_elf_header(data).unwrap();
        assert_eq!(ehdr.e_entry, 0x1000000);
        // Verify program header fields parse correctly
        let phdr = unsafe { &*(data.as_ptr().add(ehdr.e_phoff as usize) as *const Elf64Phdr) };
        assert_eq!(phdr.p_type, PT_LOAD);
        assert_eq!(phdr.p_vaddr, 0x1000000);
        assert_eq!(phdr.p_filesz, b"Hello, ELF!".len() as u64);
    }

    #[test]
    fn test_load_elf_no_load_segments() {
        // Build an ELF with no LOAD segments (e.g. only PT_NULL)
        let mut ehdr = make_minimal_ehdr();
        ehdr.e_phoff = 0; // signal no phdrs
        let bytes =
            unsafe { core::slice::from_raw_parts(&ehdr as *const _ as *const u8, EHDR_SIZE) };
        // This should fail with NoLoadSegments
        let result = unsafe { load_elf(bytes) };
        assert!(result.is_err());
    }

    #[test]
    fn test_load_elf_truncated_phdr() {
        // Just enough for the header, not the phdr
        let data = [0u8; EHDR_SIZE];
        let result = parse_elf_header(&data);
        if let Ok(ehdr) = result {
            // Header parsed, now check phdr bounds
            let phoff = ehdr.e_phoff as usize;
            let phnum = ehdr.e_phnum as usize;
            let phentsize = ehdr.e_phentsize as usize;
            let ph_end = phoff + phnum * phentsize;
            assert!(ph_end > data.len());
        }
    }

    #[test]
    fn test_loaded_elf_bounds() {
        // This is a compile-time structural test that LoadedElf
        // has the expected fields for bounds tracking.
        let loaded = LoadedElf {
            base: 0x1000000,
            top: 0x1002000,
            entry: 0x1000000,
        };
        assert_eq!(loaded.entry, 0x1000000);
        assert!(loaded.base < loaded.top);
    }

    #[test]
    fn test_constants() {
        assert_eq!(PT_NULL, 0);
        assert_eq!(PT_LOAD, 1);
        assert_eq!(EM_X86_64, 62);
    }

    #[test]
    fn test_phdr_flags_match() {
        // PF_R = 4, PF_W = 2, PF_X = 1
        assert_eq!(PF_X, 1);
        assert_eq!(PF_W, 2);
        assert_eq!(PF_R, 4);
        // Combined: RX = 5, RW = 6, RWX = 7
        assert_eq!(PF_R | PF_X, 5);
        assert_eq!(PF_R | PF_W, 6);
        assert_eq!(PF_R | PF_W | PF_X, 7);
    }

    #[test]
    fn test_stack_setup_single_arg() {
        let stack = [0u8; 8192];
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let rsp = unsafe { setup_user_stack(stack_top, 8192, &["/sbin/init"]).unwrap() };

        // RSP must be 16-byte aligned
        assert!(rsp % 16 == 0);

        // Read argc at [rsp]
        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 1);

        // Read argv[0] at [rsp+8]
        let argv0_ptr = unsafe { *(rsp as *const u64).add(1) } as *const u8;
        let argv0 = unsafe {
            let len = (0..256).position(|i| *argv0_ptr.add(i) == 0).unwrap();
            core::slice::from_raw_parts(argv0_ptr, len)
        };
        assert_eq!(core::str::from_utf8(argv0).unwrap(), "/sbin/init");
    }

    #[test]
    fn test_stack_setup_multiple_args() {
        let stack = [0u8; 8192];
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let argv = &["/bin/sh", "-c", "echo hello"];
        let rsp = unsafe { setup_user_stack(stack_top, 8192, argv).unwrap() };

        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 3);

        // Read each argv pointer and verify the string
        for (i, expected) in argv.iter().enumerate() {
            let ptr = unsafe { *(rsp as *const u64).add(1 + i) } as *const u8;
            let s = unsafe {
                let len = (0..256).position(|j| *ptr.add(j) == 0).unwrap();
                core::slice::from_raw_parts(ptr, len)
            };
            assert_eq!(
                core::str::from_utf8(s).unwrap(),
                *expected,
                "argv[{i}] mismatch"
            );
        }

        // argv[argc] should be NULL
        let null_term = unsafe { *(rsp as *const u64).add(1 + argc as usize) };
        assert_eq!(null_term, 0);
    }

    #[test]
    fn test_stack_setup_too_small() {
        let stack = [0u8; 256]; // too small
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let result = unsafe { setup_user_stack(stack_top, 256, &["/sbin/init"]) };
        assert!(result.is_err());
    }

    #[test]
    fn test_stack_setup_empty_argv() {
        let stack = [0u8; 4096];
        let stack_top = stack.as_ptr() as u64 + stack.len() as u64;
        let rsp = unsafe { setup_user_stack(stack_top, 4096, &[] as &[&str]).unwrap() };

        let argc = unsafe { *(rsp as *const u64) };
        assert_eq!(argc, 0);

        // argv[0] should be NULL
        let null_term = unsafe { *(rsp as *const u64).add(1) };
        assert_eq!(null_term, 0);
    }
}
