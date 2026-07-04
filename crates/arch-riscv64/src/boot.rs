//! RISC-V64 early boot code — FDT parsing, allocator init, MMU enable.
//!
//! Called from the assembly _start entry point. Sets up the kernel's
//! basic environment so it can run as a proper S-mode process.

#![cfg(target_arch = "riscv64")]

/// Minimal FDT (Flattened Device Tree) parser.

/// FDT header (big-endian).
#[repr(C)]
struct FdtHeader {
    magic: u32,          // 0xD00DFEED
    totalsize: u32,      // total size of DT blob
    off_dt_struct: u32,  // offset to structure block
    off_dt_strings: u32, // offset to strings block
    off_mem_rsvmap: u32, // offset to memory reserve map
    version: u32,        // should be >= 17
    last_comp_version: u32,
    boot_cpuid_phys: u32,
    size_dt_strings: u32, // size of strings block
    size_dt_struct: u32,  // size of structure block
}

const FDT_MAGIC: u32 = 0xD00DFEED;

// FDT tokens
const FDT_BEGIN_NODE: u32 = 0x00000001;
const FDT_END_NODE: u32 = 0x00000002;
const FDT_PROP: u32 = 0x00000003;
const FDT_NOP: u32 = 0x00000004;
const FDT_END: u32 = 0x00000009;

/// Read a big-endian u32 from a byte slice.
fn be_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(buf[offset..offset + 4].try_into().unwrap())
}

/// Read a big-endian u64 from a byte slice.
fn be_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(buf[offset..offset + 8].try_into().unwrap())
}

/// Parse memory information from the FDT.
/// Returns (base, size) of the first memory region found.
///
/// # Safety
///
/// `dtb` must point to a valid, accessible FDT blob.
pub unsafe fn parse_fdt_memory(dtb: *const u8) -> Option<(u64, u64)> {
    unsafe {
        // Read header
        let hdr = &*(dtb as *const FdtHeader);
        if u32::from_be(hdr.magic) != FDT_MAGIC {
            return None;
        }
        let struct_off = u32::from_be(hdr.off_dt_struct) as usize;
        let strings_off = u32::from_be(hdr.off_dt_strings) as usize;
        let totalsize = u32::from_be(hdr.totalsize) as usize;
        let dtb_slice = core::slice::from_raw_parts(dtb, totalsize);

        // Walk the structure block looking for a /memory node with a reg property
        let mut pos = struct_off;
        let mut depth = 0i32;
        let mut in_memory_node = false;
        let mut reg_addr = 0u64;
        let mut reg_size = 0u64;
        let mut reg_addr_cells = 2; // default: 2 cells for 64-bit address
        let mut reg_size_cells = 2; // default: 2 cells for 64-bit size

        while pos + 4 <= struct_off + usize::try_from(u32::from_be(hdr.size_dt_struct)).unwrap_or(0)
        {
            let token = be_u32(dtb_slice, pos);
            pos += 4;

            match token {
                FDT_BEGIN_NODE => {
                    depth += 1;
                    // Node name starts at pos, null-terminated
                    let name_start = pos;
                    while pos < dtb_slice.len() && dtb_slice[pos] != 0 {
                        pos += 1;
                    }
                    pos += 1; // skip null
                    // Align to 4 bytes
                    pos = (pos + 3) & !3;

                    // Check if this is a memory node
                    let name = core::str::from_utf8_unchecked(&dtb_slice[name_start..pos - 1]);
                    in_memory_node = name.starts_with("memory@")
                        || name == "memory"
                        || depth == 1 && name.starts_with("memory");
                }
                FDT_END_NODE => {
                    depth -= 1;
                    in_memory_node = false;
                }
                FDT_PROP => {
                    let prop_len = be_u32(dtb_slice, pos) as usize;
                    let name_off = be_u32(dtb_slice, pos + 4) as usize;
                    pos += 8;
                    let prop_data = pos;
                    pos = (pos + prop_len + 3) & !3; // align to 4

                    if in_memory_node {
                        let prop_name = core::str::from_utf8_unchecked(
                            &dtb_slice[strings_off + name_off..strings_off + name_off + 32],
                        );
                        // Trim to null terminator
                        let prop_name = prop_name.trim_end_matches('\0');

                        match prop_name {
                            "reg" if prop_len >= 8 => {
                                // Address and size are encoded using #address-cells
                                // and #size-cells from the parent node.
                                let addr_bytes = reg_addr_cells * 4;
                                let size_bytes = reg_size_cells * 4;
                                if prop_len >= addr_bytes + size_bytes {
                                    if addr_bytes == 8 {
                                        reg_addr = be_u64(dtb_slice, prop_data);
                                    } else if addr_bytes == 4 {
                                        reg_addr = be_u32(dtb_slice, prop_data) as u64;
                                    }
                                    if size_bytes == 8 {
                                        reg_size = be_u64(dtb_slice, prop_data + addr_bytes);
                                    } else if size_bytes == 4 {
                                        reg_size = be_u32(dtb_slice, prop_data + addr_bytes) as u64;
                                    }
                                    return Some((reg_addr, reg_size));
                                }
                            }
                            "#address-cells" if prop_len >= 4 => {
                                reg_addr_cells = be_u32(dtb_slice, prop_data) as usize;
                            }
                            "#size-cells" if prop_len >= 4 => {
                                reg_size_cells = be_u32(dtb_slice, prop_data) as usize;
                            }
                            _ => {}
                        }
                    }
                }
                FDT_NOP => {}
                FDT_END => break,
                _ => {
                    // Unknown token — should not happen but skip it
                }
            }
        }
        None
    }
}

/// Boot information parsed from FDT / platform knowledge.
pub struct BootInfo {
    /// Physical memory base address.
    pub mem_base: u64,
    /// Physical memory size in bytes.
    pub mem_size: u64,
    /// Kernel load address (physical).
    pub kernel_base: u64,
    /// Kernel size (approx, from link script).
    pub kernel_size: u64,
    /// Hart ID (from a0).
    pub hart_id: u64,
    /// DTB pointer (from a1).
    pub dtb_ptr: u64,
}

/// Read the SATP register.
pub fn read_satp() -> u64 {
    let satp: u64;
    unsafe {
        core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nomem, nostack));
    }
    satp
}

/// Enable SV39 paging by writing the SATP register.
///
/// # Safety
///
/// `root_ppn` must point to a valid, page-aligned root page table.
/// The page table must identity-map the kernel's code region.
pub unsafe fn enable_mmu(root_ppn: u64) {
    // SV39 mode = 8 (bits 60-63), ASID = 0 (bits 44-59), PPN = root_ppn >> 12
    let satp = (8u64 << 60) | (root_ppn >> 12);
    unsafe {
        core::arch::asm!("csrw satp, {satp}", satp = in(reg) satp, options(nomem, nostack));
        // Flush TLB after enabling paging
        core::arch::asm!("sfence.vma", options(nomem, nostack));
    }
}

/// Initialize the physical memory allocator from boot info.
///
/// # Safety
///
/// Must be called once during early boot.
pub unsafe fn init_phys_allocator(info: &BootInfo) {
    let mem_end = info.mem_base + info.mem_size;
    let kernel_end = info.kernel_base + info.kernel_size;
    let alloc_start = kernel_end.max(info.mem_base);

    if alloc_start < mem_end {
        let mut mmap = crate::alloc::PhysicalMemoryMap::new();
        mmap.add(alloc_start, mem_end);
        // SAFETY: Called once during early boot with valid memory info
        unsafe {
            crate::alloc::init_allocator(&mmap);
        }
    }
}

/// Early initialization — called from _start assembly.
///
/// # Safety
///
/// Must be called in S-mode with a0=hart_id and a1=dtb_ptr.
/// Only the boot hart should proceed; other harts should park.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn early_init(hart_id: u64, dtb_ptr: u64) {
    // Only hart 0 proceeds for now (SMP later in Phase 19.x)
    if hart_id != 0 {
        loop {
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    // Parse FDT for memory information
    let boot_info =
        if let Some((mem_base, mem_size)) = unsafe { parse_fdt_memory(dtb_ptr as *const u8) } {
            BootInfo {
                mem_base,
                mem_size,
                kernel_base: 0x80200000, // QEMU virt loads kernel here
                kernel_size: 0x100000,   // 1MB (approximate, will be refined)
                hart_id,
                dtb_ptr,
            }
        } else {
            // Fallback: assume 128MB RAM starting at 0x80000000
            BootInfo {
                mem_base: 0x80000000,
                mem_size: 128 * 1024 * 1024,
                kernel_base: 0x80200000,
                kernel_size: 0x100000,
                hart_id,
                dtb_ptr,
            }
        };

    // Initialize physical allocator
    // SAFETY: Called once during early boot with valid boot info
    unsafe {
        init_phys_allocator(&boot_info);
    }

    // Set up STVEC to point to the trap vector
    let trap_vec = crate::trap_asm::trap_vector_addr();
    unsafe {
        core::arch::asm!("csrw stvec, {addr}", addr = in(reg) trap_vec, options(nomem, nostack));
    }

    // Print a message via SBI to confirm we're alive
    for &b in b"Hello MINIX!\r\n" {
        crate::sbi::console_putchar(b);
    }

    // For now, halt after initialization
    // TODO: Set up page tables, enable MMU, load processes, switch to user
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}
