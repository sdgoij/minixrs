//! Minimal FDT (Flattened Device Tree) parser.
//!
//! Shared by the RISC-V and AArch64 boots, which both receive a DTB pointer
//! from QEMU (x0/a1) and need the RAM size from the `/memory` node before
//! sizing their physical allocators.

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
                        // Property names are NUL-terminated strings in the
                        // strings block; scan for the terminator. QEMU packs
                        // the strings block densely, so a fixed-width window
                        // would bleed into the next name and never match.
                        let name_start = strings_off + name_off;
                        let mut name_end = name_start;
                        while name_end < dtb_slice.len() && dtb_slice[name_end] != 0 {
                            name_end += 1;
                        }
                        let prop_name =
                            core::str::from_utf8_unchecked(&dtb_slice[name_start..name_end]);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_memory_reg_with_densely_packed_strings_block() {
        // Minimal FDT: root node, one memory node, and a strings block where
        // the `reg` name is immediately followed by another string (QEMU
        // packs the block densely). Reading a fixed 32-byte window from the
        // name would bleed into the next string and never match "reg".
        // `dtb` must be 4-byte aligned for the `FdtHeader` deref in the parser.
        #[repr(align(8))]
        struct Aligned([u8; 126]);
        let mut aligned = Aligned([0; 126]);
        let dtb = &mut aligned.0[..];
        let w = |b: &mut [u8], off: usize, v: u32| {
            b[off..off + 4].copy_from_slice(&v.to_be_bytes());
        };
        w(dtb, 0, 0xD00D_FEED); // magic
        w(dtb, 4, 126); // totalsize
        w(dtb, 8, 40); // off_dt_struct
        w(dtb, 12, 112); // off_dt_strings
        w(dtb, 20, 17); // version
        w(dtb, 24, 16); // last_comp_version
        w(dtb, 32, 14); // size_dt_strings
        w(dtb, 36, 72); // size_dt_struct

        let mut pos = 40;
        let tok = |b: &mut [u8], p: &mut usize, t: u32, name: &[u8]| {
            b[*p..*p + 4].copy_from_slice(&t.to_be_bytes());
            *p += 4;
            b[*p..*p + name.len()].copy_from_slice(name);
            *p += name.len() + 1; // NUL terminator
            *p = (*p + 3) & !3;
        };
        tok(dtb, &mut pos, 1, b""); // root node
        tok(dtb, &mut pos, 1, b"memory@80000000");
        // reg property: addr 0x80000000, size 0x10000000
        dtb[pos..pos + 4].copy_from_slice(&3u32.to_be_bytes()); // PROP
        dtb[pos + 4..pos + 8].copy_from_slice(&16u32.to_be_bytes()); // prop_len
        dtb[pos + 8..pos + 12].copy_from_slice(&0u32.to_be_bytes()); // name_off -> "reg"
        dtb[pos + 12..pos + 20].copy_from_slice(&0x8000_0000u64.to_be_bytes());
        dtb[pos + 20..pos + 28].copy_from_slice(&0x1000_0000u64.to_be_bytes());
        pos += 28;
        for _ in 0..2 {
            dtb[pos..pos + 4].copy_from_slice(&2u32.to_be_bytes()); // END_NODE
            pos += 4;
        }
        dtb[pos..pos + 4].copy_from_slice(&9u32.to_be_bytes()); // FDT_END
        // strings block @ 112: "reg" immediately followed by another string
        dtb[112..116].copy_from_slice(b"reg\0");
        dtb[116..].copy_from_slice(b"always-on\0");

        let info = unsafe { parse_fdt_memory(dtb.as_ptr()) };
        assert_eq!(info, Some((0x8000_0000, 0x1000_0000)), "got {info:?}");
    }
}
