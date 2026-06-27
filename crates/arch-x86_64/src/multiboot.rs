//! Multiboot structures — adapted from i386 `multiboot.h`
//!
//! These are the same on x86_64 as on i386 (the multiboot spec is 32-bit
//! but works with a 32-bit trampoline). We keep the C-compatible layouts.

use core::fmt;

// ── Multiboot header constants ──────────────────────────────────────────

pub const MULTIBOOT_HEADER_MAGIC: u32 = 0x1BADB002;
pub const MULTIBOOT_BOOTLOADER_MAGIC: u32 = 0x2BADB002;
pub const MULTIBOOT_MOD_ALIGN: u32 = 0x00001000;
pub const MULTIBOOT_INFO_ALIGN: u32 = 0x00000004;

// ── Multiboot header flags ──────────────────────────────────────────────

pub const MULTIBOOT_PAGE_ALIGN: u32 = 0x00000001;
pub const MULTIBOOT_MEMORY_INFO: u32 = 0x00000002;
pub const MULTIBOOT_VIDEO_MODE: u32 = 0x00000004;
pub const MULTIBOOT_AOUT_KLUDGE: u32 = 0x00010000;

// ── Multiboot info flags ────────────────────────────────────────────────

pub const MULTIBOOT_INFO_MEMORY: u32 = 0x00000001;
pub const MULTIBOOT_INFO_BOOTDEV: u32 = 0x00000002;
pub const MULTIBOOT_INFO_CMDLINE: u32 = 0x00000004;
pub const MULTIBOOT_INFO_MODS: u32 = 0x00000008;
pub const MULTIBOOT_INFO_AOUT_SYMS: u32 = 0x00000010;
pub const MULTIBOOT_INFO_ELF_SHDR: u32 = 0x00000020;
pub const MULTIBOOT_INFO_MEM_MAP: u32 = 0x00000040;
pub const MULTIBOOT_INFO_DRIVE_INFO: u32 = 0x00000080;
pub const MULTIBOOT_INFO_CONFIG_TABLE: u32 = 0x00000100;
pub const MULTIBOOT_INFO_BOOT_LOADER_NAME: u32 = 0x00000200;
pub const MULTIBOOT_INFO_APM_TABLE: u32 = 0x00000400;
pub const MULTIBOOT_INFO_VBE_INFO: u32 = 0x00000800;
pub const MULTIBOOT_INFO_FRAMEBUFFER_INFO: u32 = 0x00001000;

// ── Memory map entry types ──────────────────────────────────────────────

pub const MULTIBOOT_MEMORY_AVAILABLE: u32 = 1;
pub const MULTIBOOT_MEMORY_RESERVED: u32 = 2;
pub const MULTIBOOT_MEMORY_ACPI_RECLAIMABLE: u32 = 3;
pub const MULTIBOOT_MEMORY_NVS: u32 = 4;
pub const MULTIBOOT_MEMORY_BADRAM: u32 = 5;

// ── Structures ──────────────────────────────────────────────────────────

/// Multiboot header (in boot image).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MultibootHeader {
    pub magic: u32,
    pub flags: u32,
    pub checksum: u32,
    pub header_addr: u32,
    pub load_addr: u32,
    pub load_end_addr: u32,
    pub bss_end_addr: u32,
    pub entry_addr: u32,
    pub mode_type: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

/// Multiboot info structure (passed by bootloader).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MultibootInfo {
    pub flags: u32,
    pub mem_lower: u32,
    pub mem_upper: u32,
    pub boot_device: u32,
    pub cmdline: u32,
    pub mods_count: u32,
    pub mods_addr: u32,
    pub syms: [u32; 4],
    pub mmap_length: u32,
    pub mmap_addr: u32,
    pub drives_length: u32,
    pub drives_addr: u32,
    pub config_table: u32,
    pub boot_loader_name: u32,
    pub apm_table: u32,
    pub vbe_control_info: u32,
    pub vbe_mode_info: u32,
    pub vbe_mode: u16,
    pub vbe_interface_seg: u16,
    pub vbe_interface_off: u16,
    pub vbe_interface_len: u16,
    pub framebuffer_addr: u64,
    pub framebuffer_pitch: u32,
    pub framebuffer_width: u32,
    pub framebuffer_height: u32,
    pub framebuffer_bpp: u8,
    pub framebuffer_type: u8,
    pub color_info: [u8; 6],
}

impl fmt::Debug for MultibootInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultibootInfo")
            .field("flags", &self.flags)
            .field("mem_lower", &self.mem_lower)
            .field("mem_upper", &self.mem_upper)
            .field("mods_count", &self.mods_count)
            .finish()
    }
}

/// Multiboot memory map entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MultibootMmapEntry {
    pub size: u32,
    pub addr: u64,
    pub len: u64,
    pub typ: u32,
}

/// Multiboot module info.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MultibootModInfo {
    pub mod_start: u32,
    pub mod_end: u32,
    pub cmdline: u32,
    pub pad: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_multiboot_header_size() {
        assert_eq!(size_of::<MultibootHeader>(), 48);
    }

    #[test]
    fn test_multiboot_info_size() {
        // Multiboot info is variable length; flags determine what's valid
        assert!(size_of::<MultibootInfo>() >= 88);
    }

    #[test]
    fn test_multiboot_mmap_entry() {
        assert!(size_of::<MultibootMmapEntry>() <= 32);
    }

    #[test]
    fn test_constants() {
        assert_eq!(MULTIBOOT_HEADER_MAGIC, 0x1BADB002);
        assert_eq!(MULTIBOOT_BOOTLOADER_MAGIC, 0x2BADB002);
        assert_eq!(MULTIBOOT_INFO_MEM_MAP, 0x00000040);
    }

    #[test]
    fn test_all_header_constants() {
        assert_eq!(MULTIBOOT_MOD_ALIGN, 0x00001000);
        assert_eq!(MULTIBOOT_INFO_ALIGN, 0x00000004);
    }

    #[test]
    fn test_all_header_flags() {
        assert_eq!(MULTIBOOT_PAGE_ALIGN, 0x00000001);
        assert_eq!(MULTIBOOT_MEMORY_INFO, 0x00000002);
        assert_eq!(MULTIBOOT_VIDEO_MODE, 0x00000004);
        assert_eq!(MULTIBOOT_AOUT_KLUDGE, 0x00010000);
    }

    #[test]
    fn test_all_info_flags() {
        assert_eq!(MULTIBOOT_INFO_MEMORY, 0x00000001);
        assert_eq!(MULTIBOOT_INFO_BOOTDEV, 0x00000002);
        assert_eq!(MULTIBOOT_INFO_CMDLINE, 0x00000004);
        assert_eq!(MULTIBOOT_INFO_MODS, 0x00000008);
        assert_eq!(MULTIBOOT_INFO_AOUT_SYMS, 0x00000010);
        assert_eq!(MULTIBOOT_INFO_ELF_SHDR, 0x00000020);
        assert_eq!(MULTIBOOT_INFO_MEM_MAP, 0x00000040);
        assert_eq!(MULTIBOOT_INFO_DRIVE_INFO, 0x00000080);
        assert_eq!(MULTIBOOT_INFO_CONFIG_TABLE, 0x00000100);
        assert_eq!(MULTIBOOT_INFO_BOOT_LOADER_NAME, 0x00000200);
        assert_eq!(MULTIBOOT_INFO_APM_TABLE, 0x00000400);
        assert_eq!(MULTIBOOT_INFO_VBE_INFO, 0x00000800);
        assert_eq!(MULTIBOOT_INFO_FRAMEBUFFER_INFO, 0x00001000);
    }

    #[test]
    fn test_all_memory_types() {
        assert_eq!(MULTIBOOT_MEMORY_AVAILABLE, 1);
        assert_eq!(MULTIBOOT_MEMORY_RESERVED, 2);
        assert_eq!(MULTIBOOT_MEMORY_ACPI_RECLAIMABLE, 3);
        assert_eq!(MULTIBOOT_MEMORY_NVS, 4);
        assert_eq!(MULTIBOOT_MEMORY_BADRAM, 5);
    }

    #[test]
    fn test_multiboot_mmap_entry_exact_size() {
        // size(4) + pad(4) + addr(8) + len(8) + typ(4) + pad(4) = 32.
        // The u64 fields require 8-byte alignment, adding padding.
        assert_eq!(size_of::<MultibootMmapEntry>(), 32);
    }

    #[test]
    fn test_multiboot_mod_info_size() {
        // mod_start(4) + mod_end(4) + cmdline(4) + pad(4) = 16.
        assert_eq!(size_of::<MultibootModInfo>(), 16);
    }

    // ── Field offset tests using offset_of! ────────────────────────────────

    #[test]
    fn test_multiboot_header_offsets() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(MultibootHeader, magic), 0);
        assert_eq!(offset_of!(MultibootHeader, flags), 4);
        assert_eq!(offset_of!(MultibootHeader, checksum), 8);
        assert_eq!(offset_of!(MultibootHeader, header_addr), 12);
        assert_eq!(offset_of!(MultibootHeader, load_addr), 16);
        assert_eq!(offset_of!(MultibootHeader, load_end_addr), 20);
        assert_eq!(offset_of!(MultibootHeader, bss_end_addr), 24);
        assert_eq!(offset_of!(MultibootHeader, entry_addr), 28);
        assert_eq!(offset_of!(MultibootHeader, mode_type), 32);
        assert_eq!(offset_of!(MultibootHeader, width), 36);
        assert_eq!(offset_of!(MultibootHeader, height), 40);
        assert_eq!(offset_of!(MultibootHeader, depth), 44);
    }

    #[test]
    fn test_multiboot_info_offsets() {
        use core::mem::offset_of;
        // First few fields (up to the variable-length section).
        assert_eq!(offset_of!(MultibootInfo, flags), 0);
        assert_eq!(offset_of!(MultibootInfo, mem_lower), 4);
        assert_eq!(offset_of!(MultibootInfo, mem_upper), 8);
        assert_eq!(offset_of!(MultibootInfo, boot_device), 12);
        assert_eq!(offset_of!(MultibootInfo, cmdline), 16);
        assert_eq!(offset_of!(MultibootInfo, mods_count), 20);
        assert_eq!(offset_of!(MultibootInfo, mods_addr), 24);
        // syms is [u32; 4] at offset 28.
        assert_eq!(offset_of!(MultibootInfo, syms), 28);
        // mmap fields.
        assert_eq!(offset_of!(MultibootInfo, mmap_length), 44);
        assert_eq!(offset_of!(MultibootInfo, mmap_addr), 48);
    }

    #[test]
    fn test_multiboot_mmap_entry_offsets() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(MultibootMmapEntry, size), 0);
        // addr is u64, so it's 8-byte aligned (offset 8, not 4).
        assert_eq!(offset_of!(MultibootMmapEntry, addr), 8);
        assert_eq!(offset_of!(MultibootMmapEntry, len), 16);
        assert_eq!(offset_of!(MultibootMmapEntry, typ), 24);
    }

    #[test]
    fn test_multiboot_mod_info_offsets() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(MultibootModInfo, mod_start), 0);
        assert_eq!(offset_of!(MultibootModInfo, mod_end), 4);
        assert_eq!(offset_of!(MultibootModInfo, cmdline), 8);
        assert_eq!(offset_of!(MultibootModInfo, pad), 12);
    }
}
