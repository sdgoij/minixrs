//! MMC/SD card block driver.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/mmc/`
//!
//! Supports SD and MMC cards via an abstracted host controller.
//! The SDHCI host is the standard for x86_64; OMAP MMCHS is ARM-only
//! and deferred.

#![allow(clippy::identity_op)]

use crate::DriverError;

// ═══════════════════════════════════════════════════════════════════════════
// SD/MMC Protocol Constants (from sdmmcreg.h)
// ═══════════════════════════════════════════════════════════════════════════

// ── MMC Commands ─────────────────────────────────────────────────────────

pub const MMC_GO_IDLE_STATE: u32 = 0;
pub const MMC_SEND_OP_COND: u32 = 1;
pub const MMC_ALL_SEND_CID: u32 = 2;
pub const MMC_SET_RELATIVE_ADDR: u32 = 3;
pub const MMC_SWITCH: u32 = 6;
pub const MMC_SELECT_CARD: u32 = 7;
pub const MMC_SEND_EXT_CSD: u32 = 8;
pub const MMC_SEND_CSD: u32 = 9;
pub const MMC_SEND_CID: u32 = 10;
pub const MMC_STOP_TRANSMISSION: u32 = 12;
pub const MMC_SEND_STATUS: u32 = 13;
pub const MMC_INACTIVE_STATE: u32 = 15;
pub const MMC_SET_BLOCKLEN: u32 = 16;
pub const MMC_READ_BLOCK_SINGLE: u32 = 17;
pub const MMC_READ_BLOCK_MULTIPLE: u32 = 18;
pub const MMC_SET_BLOCK_COUNT: u32 = 23;
pub const MMC_WRITE_BLOCK_SINGLE: u32 = 24;
pub const MMC_WRITE_BLOCK_MULTIPLE: u32 = 25;
pub const MMC_PROGRAM_CSD: u32 = 27;
pub const MMC_SET_WRITE_PROT: u32 = 28;
pub const MMC_SET_CLR_WRITE_PROT: u32 = 29;
pub const MMC_SET_SEND_WRITE_PROT: u32 = 30;
pub const MMC_TAG_SECTOR_START: u32 = 32;
pub const MMC_TAG_SECTOR_END: u32 = 33;
pub const MMC_UNTAG_SECTOR: u32 = 34;
pub const MMC_TAG_ERASE_GROUP_START: u32 = 35;
pub const MMC_TAG_ERASE_GROUP_END: u32 = 36;
pub const MMC_UNTAG_ERASE_GROUP: u32 = 37;
pub const MMC_ERASE: u32 = 38;
pub const MMC_LOCK_UNLOCK: u32 = 42;
pub const MMC_APP_CMD: u32 = 55;
pub const MMC_READ_OCR: u32 = 58;

// ── SD Commands ──────────────────────────────────────────────────────────

pub const SD_SEND_RELATIVE_ADDR: u32 = 3;
pub const SD_SEND_SWITCH_FUNC: u32 = 6;
pub const SD_SEND_IF_COND: u32 = 8;

// ── SD Application Commands ───────────────────────────────────────────────

pub const SD_APP_SET_BUS_WIDTH: u32 = 6;
pub const SD_APP_OP_COND: u32 = 41;
pub const SD_APP_SEND_SCR: u32 = 51;

// ── OCR Bits ─────────────────────────────────────────────────────────────

pub const MMC_OCR_MEM_READY: u32 = 1 << 31;
pub const MMC_OCR_HCS: u32 = 1 << 30;
pub const MMC_OCR_3_5V_3_6V: u32 = 1 << 23;
pub const MMC_OCR_3_4V_3_5V: u32 = 1 << 22;
pub const MMC_OCR_3_3V_3_4V: u32 = 1 << 21;
pub const MMC_OCR_3_2V_3_3V: u32 = 1 << 20;
pub const MMC_OCR_3_1V_3_2V: u32 = 1 << 19;
pub const MMC_OCR_3_0V_3_1V: u32 = 1 << 18;
pub const MMC_OCR_2_9V_3_0V: u32 = 1 << 17;
pub const MMC_OCR_2_8V_2_9V: u32 = 1 << 16;
pub const MMC_OCR_2_7V_2_8V: u32 = 1 << 15;
pub const MMC_OCR_2_6V_2_7V: u32 = 1 << 14;
pub const MMC_OCR_2_5V_2_6V: u32 = 1 << 13;
pub const MMC_OCR_1_6V_1_7V: u32 = 1 << 4;

// ── R1 Response Bits ─────────────────────────────────────────────────────

pub const MMC_R1_READY_FOR_DATA: u32 = 1 << 8;
pub const MMC_R1_APP_CMD: u32 = 1 << 5;

// ── Response Decoding ────────────────────────────────────────────────────

pub fn mmc_r1(resp: &[u32; 4]) -> u32 {
    resp[0]
}
pub fn mmc_r3(resp: &[u32; 4]) -> u32 {
    resp[0]
}
pub fn sd_r6(resp: &[u32; 4]) -> u32 {
    resp[0]
}

// ── RCA ──────────────────────────────────────────────────────────────────

pub fn mmc_arg_rca(rca: u16) -> u32 {
    (rca as u32) << 16
}
pub fn sd_r6_rca(resp: &[u32; 4]) -> u16 {
    (sd_r6(resp) >> 16) as u16
}

// ── Bus Width ────────────────────────────────────────────────────────────

pub const SD_ARG_BUS_WIDTH_1: u32 = 0;
pub const SD_ARG_BUS_WIDTH_4: u32 = 2;

// ── EXT_CSD Fields ──────────────────────────────────────────────────────

pub const EXT_CSD_BUS_WIDTH: u32 = 183;
pub const EXT_CSD_HS_TIMING: u32 = 185;
pub const EXT_CSD_REV: u32 = 192;
pub const EXT_CSD_STRUCTURE: u32 = 194;
pub const EXT_CSD_CARD_TYPE: u32 = 196;
pub const EXT_CSD_CMD_SET_NORMAL: u32 = 1 << 0;
pub const EXT_CSD_BUS_WIDTH_1: u8 = 0;
pub const EXT_CSD_BUS_WIDTH_4: u8 = 1;
pub const EXT_CSD_BUS_WIDTH_8: u8 = 2;
pub const EXT_CSD_STRUCTURE_VER_1_0: u8 = 0;
pub const EXT_CSD_STRUCTURE_VER_1_1: u8 = 1;
pub const EXT_CSD_STRUCTURE_VER_1_2: u8 = 2;
pub const EXT_CSD_CARD_TYPE_26M: u32 = 1 << 0;
pub const EXT_CSD_CARD_TYPE_52M: u32 = 1 << 1;

// ── MMC_SWITCH Access Modes ──────────────────────────────────────────────

pub const MMC_SWITCH_MODE_CMD_SET: u8 = 0x00;
pub const MMC_SWITCH_MODE_SET_BITS: u8 = 0x01;
pub const MMC_SWITCH_MODE_CLEAR_BITS: u8 = 0x02;
pub const MMC_SWITCH_MODE_WRITE_BYTE: u8 = 0x03;

// ── CSD Field Accessors ──────────────────────────────────────────────────

/// Extract a bitfield from a 128-bit response (4 × u32).
/// Corresponds to C `MMC_RSP_BITS` / `__bitfield`.
pub fn mmc_rsp_bits(resp: &[u32; 4], start: i32, len: i32) -> u32 {
    if start < 0 || !(0..=32).contains(&len) {
        return 0;
    }
    let mut dst: u32 = 0;
    let mut s = start;
    let mut l = len;
    let mut shift = 0;
    while l > 0 {
        let byte_idx = (s / 8) as usize;
        let bit_shift = (s % 8) as u32;
        let bc = (8 - bit_shift).min(l as u32);
        let byte = if byte_idx < 16 {
            // The response array is in units of u32 (4 bytes each).
            let word_idx = byte_idx / 4;
            let byte_in_word = byte_idx % 4;
            (resp[word_idx].to_le_bytes())[byte_in_word]
        } else {
            0
        };
        dst |= ((byte >> bit_shift) as u32) << shift;
        shift += bc;
        s += bc as i32;
        l -= bc as i32;
    }
    let mask = if len == 32 { !0u32 } else { (1u32 << len) - 1 };
    dst & mask
}

// ── CSD Decode (MMC) ────────────────────────────────────────────────────

pub fn mmc_csd_csdver(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 126, 2)
}
pub const MMC_CSD_CSDVER_1_0: u32 = 0;
pub const MMC_CSD_CSDVER_1_1: u32 = 1;
pub const MMC_CSD_CSDVER_1_2: u32 = 2;
pub const MMC_CSD_CSDVER_EXT_CSD: u32 = 3;

pub fn mmc_csd_mmcver(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 122, 4)
}
pub const MMC_CSD_MMCVER_1_0: u32 = 0;
pub const MMC_CSD_MMCVER_4_0: u32 = 4;

pub fn mmc_csd_c_size(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 62, 12)
}
pub fn mmc_csd_c_size_mult(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 47, 3)
}
pub fn mmc_csd_capacity(resp: &[u32; 4]) -> u32 {
    (mmc_csd_c_size(resp) + 1) << (mmc_csd_c_size_mult(resp) + 2)
}
pub fn mmc_csd_read_bl_len(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 80, 4)
}
pub fn mmc_csd_write_bl_len(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 22, 4)
}

// ── CSD Decode (SD) ─────────────────────────────────────────────────────

pub fn sd_csd_c_size(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 62, 12)
}
pub fn sd_csd_c_size_mult(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 47, 3)
}
pub fn sd_csd_capacity(resp: &[u32; 4]) -> u32 {
    (sd_csd_c_size(resp) + 1) << (sd_csd_c_size_mult(resp) + 2)
}
pub fn sd_csd_v2_c_size(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 48, 22)
}
pub fn sd_csd_v2_capacity(resp: &[u32; 4]) -> u32 {
    (sd_csd_v2_c_size(resp) + 1) << 10
}
pub fn sd_csd_read_bl_len(resp: &[u32; 4]) -> u32 {
    mmc_rsp_bits(resp, 80, 4)
}
pub const SD_CSD_RW_BL_LEN_1G: u32 = 0x9;

// ── SCR Decode ──────────────────────────────────────────────────────────

pub fn scr_structure(scr: &[u32; 2]) -> u32 {
    mmc_rsp_bits_from(scr, 60, 4)
}
pub fn scr_sd_spec(scr: &[u32; 2]) -> u32 {
    mmc_rsp_bits_from(scr, 56, 4)
}
pub fn scr_sd_bus_widths(scr: &[u32; 2]) -> u32 {
    mmc_rsp_bits_from(scr, 48, 4)
}
pub const SCR_SD_BUS_WIDTHS_1BIT: u32 = 1 << 0;
pub const SCR_SD_BUS_WIDTHS_4BIT: u32 = 1 << 2;

fn mmc_rsp_bits_from<T: AsRef<[u32]>>(resp: T, start: i32, len: i32) -> u32 {
    let r = resp.as_ref();
    if r.len() < 4 {
        let mut buf = [0u32; 4];
        for (i, v) in r.iter().enumerate() {
            buf[i] = *v;
        }
        return mmc_rsp_bits(&buf, start, len);
    }
    let mut buf = [0u32; 4];
    for (i, v) in r.iter().take(4).enumerate() {
        buf[i] = *v;
    }
    mmc_rsp_bits(&buf, start, len)
}

// ═══════════════════════════════════════════════════════════════════════════
// SDHCI Register Definitions (from sdhcreg.h)
// ═══════════════════════════════════════════════════════════════════════════

pub const SDHC_DMA_ADDR: u16 = 0x00;
pub const SDHC_BLOCK_SIZE: u16 = 0x04;
pub const SDHC_BLOCK_COUNT: u16 = 0x06;
pub const SDHC_BLOCK_COUNT_MAX: u16 = 512;
pub const SDHC_ARGUMENT: u16 = 0x08;
pub const SDHC_TRANSFER_MODE: u16 = 0x0c;
pub const SDHC_MULTI_BLOCK_MODE: u32 = 1 << 5;
pub const SDHC_READ_MODE: u32 = 1 << 4;
pub const SDHC_AUTO_CMD12_ENABLE: u32 = 1 << 2;
pub const SDHC_BLOCK_COUNT_ENABLE: u32 = 1 << 1;
pub const SDHC_DMA_ENABLE: u32 = 1 << 0;
pub const SDHC_COMMAND: u16 = 0x0e;
pub const SDHC_DATA_PRESENT_SELECT: u32 = 1 << 5;
pub const SDHC_INDEX_CHECK_ENABLE: u32 = 1 << 4;
pub const SDHC_CRC_CHECK_ENABLE: u32 = 1 << 3;
pub const SDHC_NO_RESPONSE: u32 = 0 << 0;
pub const SDHC_RESP_LEN_136: u32 = 1 << 0;
pub const SDHC_RESP_LEN_48: u32 = 2 << 0;
pub const SDHC_RESP_LEN_48_CHK_BUSY: u32 = 3 << 0;
pub const SDHC_RESPONSE: u16 = 0x10;
pub const SDHC_DATA: u16 = 0x20;
pub const SDHC_PRESENT_STATE: u16 = 0x24;
pub const SDHC_CARD_INSERTED: u32 = 1 << 16;
pub const SDHC_BUFFER_READ_ENABLE: u32 = 1 << 11;
pub const SDHC_BUFFER_WRITE_ENABLE: u32 = 1 << 10;
pub const SDHC_READ_TRANSFER_ACTIVE: u32 = 1 << 9;
pub const SDHC_WRITE_TRANSFER_ACTIVE: u32 = 1 << 8;
pub const SDHC_CMD_INHIBIT_DAT: u32 = 1 << 1;
pub const SDHC_CMD_INHIBIT_CMD: u32 = 1 << 0;
pub const SDHC_CMD_INHIBIT_MASK: u32 = 0x0003;
pub const SDHC_HOST_CTL: u16 = 0x28;
pub const SDHC_POWER_CTL: u16 = 0x29;
pub const SDHC_CLOCK_CTL: u16 = 0x2c;
pub const SDHC_SOFTWARE_RESET: u16 = 0x2f;
pub const SDHC_RESET_ALL: u32 = 1 << 0;
pub const SDHC_NINTR_STATUS: u16 = 0x30;
pub const SDHC_ERROR_INTERRUPT: u32 = 1 << 15;
pub const SDHC_CARD_INTERRUPT: u32 = 1 << 8;
pub const SDHC_TRANSFER_COMPLETE: u32 = 1 << 1;
pub const SDHC_COMMAND_COMPLETE: u32 = 1 << 0;
pub const SDHC_EINTR_STATUS: u16 = 0x32;
pub const SDHC_NINTR_STATUS_EN: u16 = 0x34;
pub const SDHC_EINTR_STATUS_EN: u16 = 0x36;
pub const SDHC_NINTR_SIGNAL_EN: u16 = 0x38;
pub const SDHC_EINTR_SIGNAL_EN: u16 = 0x3a;
pub const SDHC_CAPABILITIES: u16 = 0x40;
pub const SDHC_HOST_VER: u16 = 0xFC;

// ═══════════════════════════════════════════════════════════════════════════
// Host Controller Types (from mmchost.h)
// ═══════════════════════════════════════════════════════════════════════════

pub const MAX_SD_SLOTS: usize = 4;
pub const SUBPARTITION_PER_PARTITION: usize = 4;
pub const PARTITIONS_PER_DISK: usize = 4;
pub const MINOR_PER_DISK: usize = 1;
pub const DEV_PER_DRIVE: usize = MINOR_PER_DISK + PARTITIONS_PER_DISK; // 5
pub const SUB_PER_DRIVE: usize = PARTITIONS_PER_DISK * SUBPARTITION_PER_PARTITION; // 16

pub const SD_MODE_UNINITIALIZED: u32 = 0;
pub const SD_MODE_CARD_IDENTIFICATION: u32 = 1;
pub const SD_MODE_DATA_TRANSFER_MODE: u32 = 2;

pub const RESP_LEN_48_CHK_BUSY: u32 = 3 << 0;
pub const RESP_LEN_48: u32 = 2 << 0;
pub const RESP_LEN_136: u32 = 1 << 0;
pub const RESP_NO_RESPONSE: u32 = 0 << 0;

pub const DATA_NONE: u32 = 0;
pub const DATA_READ: u32 = 1;
pub const DATA_WRITE: u32 = 2;

/// SD card registers decoded from R2 responses.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SdCardRegs {
    pub cid: [u32; 4],
    pub rca: u32,
    pub dsr: u32,
    pub csd: [u32; 4],
    pub scr: [u32; 2],
    pub ocr: u32,
    pub ssr: [u32; 5],
    pub csr: u32,
}

/// An MMC/SD command descriptor.
#[derive(Debug)]
#[repr(C)]
pub struct MmcCommand {
    pub cmd: u32,
    pub args: u32,
    pub resp_type: u32,
    pub data_type: u32,
    pub resp: [u32; 4],
    pub data: Option<&'static mut [u8]>,
    pub data_len: u32,
}

impl MmcCommand {
    pub const fn new() -> Self {
        Self {
            cmd: 0,
            args: 0,
            resp_type: 0,
            data_type: 0,
            resp: [0; 4],
            data: None,
            data_len: 0,
        }
    }
}

impl Default for MmcCommand {
    fn default() -> Self {
        Self::new()
    }
}

/// Card state flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    Initial = 0,
    Identified = 1,
    Deaf = 2,
    Dead = 3,
    Disconnected = 4,
}

/// An SD/MMC card descriptor.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct SdCard {
    pub slot_idx: usize,
    pub regs: SdCardRegs,
    pub blk_size: u32,
    pub blk_count: u32,
    pub card_state: CardState,
    pub open_ct: i32,
    pub block_size: u32,
}

impl SdCard {
    pub const fn new() -> Self {
        Self {
            slot_idx: 0,
            regs: SdCardRegs {
                cid: [0; 4],
                rca: 0,
                dsr: 0,
                csd: [0; 4],
                scr: [0; 2],
                ocr: 0,
                ssr: [0; 5],
                csr: 0,
            },
            blk_size: 512,
            blk_count: 0,
            card_state: CardState::Initial,
            open_ct: 0,
            block_size: 512,
        }
    }
}

/// An SD slot descriptor.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct SdSlot {
    pub host_idx: usize,
    pub state: u32,
    pub card: SdCard,
    pub card_present: bool,
}

impl SdSlot {
    pub const fn new(host_idx: usize) -> Self {
        Self {
            host_idx,
            state: SD_MODE_UNINITIALIZED,
            card: SdCard::new(),
            card_present: false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Host Controller Trait
// ═══════════════════════════════════════════════════════════════════════════

/// Abstract MMC host controller operations.
///
/// Corresponds to the C `struct mmc_host` function pointers.
pub trait MmcHost {
    /// Reset the host controller.
    fn reset(&mut self) -> Result<(), DriverError>;
    /// Detect if a card is present in the given slot.
    fn card_detect(&self, slot: &SdSlot) -> bool;
    /// Initialize a detected card (identify, get CSD/CID, set RCA).
    fn card_initialize(&mut self, slot: &mut SdSlot) -> Result<(), DriverError>;
    /// Release a card.
    fn card_release(&mut self, card: &mut SdCard) -> Result<(), DriverError>;
    /// Read `count` blocks starting at `blknr` into `buf`.
    fn read(
        &mut self,
        card: &SdCard,
        blknr: u32,
        count: u32,
        buf: &mut [u8],
    ) -> Result<(), DriverError>;
    /// Write `count` blocks starting at `blknr` from `buf`.
    fn write(
        &mut self,
        card: &SdCard,
        blknr: u32,
        count: u32,
        buf: &[u8],
    ) -> Result<(), DriverError>;
    /// Handle a hardware interrupt.
    fn handle_intr(&mut self, irqs: u32);
}

// ═══════════════════════════════════════════════════════════════════════════
// Dummy Host (for testing)
// ═══════════════════════════════════════════════════════════════════════════

/// A dummy host controller that simulates a 512 MB SD card.
pub struct DummyHost {
    pub card_present: bool,
}

impl DummyHost {
    pub fn new() -> Self {
        Self { card_present: true }
    }
}

impl MmcHost for DummyHost {
    fn reset(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn card_detect(&self, _slot: &SdSlot) -> bool {
        self.card_present
    }

    fn card_initialize(&mut self, slot: &mut SdSlot) -> Result<(), DriverError> {
        slot.card.card_state = CardState::Identified;
        slot.card.blk_size = 512;
        slot.card.blk_count = 1024 * 1024; // 512 MB
        Ok(())
    }

    fn card_release(&mut self, card: &mut SdCard) -> Result<(), DriverError> {
        card.card_state = CardState::Disconnected;
        Ok(())
    }

    fn read(
        &mut self,
        _card: &SdCard,
        _blknr: u32,
        _count: u32,
        buf: &mut [u8],
    ) -> Result<(), DriverError> {
        buf.fill(0);
        Ok(())
    }

    fn write(
        &mut self,
        _card: &SdCard,
        _blknr: u32,
        _count: u32,
        _buf: &[u8],
    ) -> Result<(), DriverError> {
        Ok(())
    }

    fn handle_intr(&mut self, _irqs: u32) {}
}

// ═══════════════════════════════════════════════════════════════════════════
// Block Driver API
// ═══════════════════════════════════════════════════════════════════════════

/// Global MMC state placeholder.
/// Note: In the full implementation, this would hold the host controller
/// and slot state, but the host controller abstraction requires
/// allocation or static storage not yet set up.
pub struct MMCState {
    pub initialized: bool,
}

impl MMCState {
    pub const fn new() -> Self {
        Self { initialized: false }
    }
}

// MMC state is not behind a static mut; it's created on init.

impl Default for SdCard {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for DummyHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for MMCState {
    fn default() -> Self {
        Self::new()
    }
}

/// Open an MMC block device by minor number.
pub fn mmc_open(minor: usize, _access: i32) -> Result<(), DriverError> {
    if minor > 0 {
        return Err(DriverError::NotFound);
    }
    todo!("mmc_open needs slot/card state and partition support; see PORTING_PLAN.md 12.17")
}

/// Close an MMC block device.
pub fn mmc_close(minor: usize) -> Result<(), DriverError> {
    if minor > 0 {
        return Err(DriverError::NotFound);
    }
    todo!("mmc_close needs slot/card state; see PORTING_PLAN.md 12.17")
}

/// Read or write blocks on an MMC device.
pub fn mmc_transfer(
    _minor: usize,
    _do_write: bool,
    _position: u64,
    _buf: &mut [u8],
) -> Result<usize, DriverError> {
    todo!("mmc_transfer needs SDHCI host + slot state; see PORTING_PLAN.md 12.17")
}

/// Block size for MMC devices (always 512 in this driver).
pub const MMC_BLOCK_SIZE: u32 = 512;

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constants ──────────────────────────────────────────────────────

    #[test]
    fn test_mmc_commands() {
        assert_eq!(MMC_GO_IDLE_STATE, 0);
        assert_eq!(MMC_SEND_OP_COND, 1);
        assert_eq!(MMC_ALL_SEND_CID, 2);
        assert_eq!(MMC_SET_RELATIVE_ADDR, 3);
        assert_eq!(MMC_SEND_CSD, 9);
        assert_eq!(MMC_SEND_CID, 10);
        assert_eq!(MMC_STOP_TRANSMISSION, 12);
        assert_eq!(MMC_SEND_STATUS, 13);
        assert_eq!(MMC_SET_BLOCKLEN, 16);
        assert_eq!(MMC_READ_BLOCK_SINGLE, 17);
        assert_eq!(MMC_READ_BLOCK_MULTIPLE, 18);
        assert_eq!(MMC_WRITE_BLOCK_SINGLE, 24);
        assert_eq!(MMC_WRITE_BLOCK_MULTIPLE, 25);
        assert_eq!(MMC_APP_CMD, 55);
        assert_eq!(MMC_READ_OCR, 58);
    }

    #[test]
    fn test_sd_commands() {
        assert_eq!(SD_SEND_RELATIVE_ADDR, 3);
        assert_eq!(SD_SEND_SWITCH_FUNC, 6);
        assert_eq!(SD_SEND_IF_COND, 8);
    }

    #[test]
    fn test_sd_app_commands() {
        assert_eq!(SD_APP_SET_BUS_WIDTH, 6);
        assert_eq!(SD_APP_OP_COND, 41);
        assert_eq!(SD_APP_SEND_SCR, 51);
    }

    #[test]
    fn test_ocr_bits() {
        assert_eq!(MMC_OCR_MEM_READY, 0x8000_0000);
        assert_eq!(MMC_OCR_HCS, 0x4000_0000);
    }

    #[test]
    fn test_r1_bits() {
        assert_eq!(MMC_R1_READY_FOR_DATA, 0x100);
        assert_eq!(MMC_R1_APP_CMD, 0x20);
    }

    // ── Response Decoding ──────────────────────────────────────────────

    #[test]
    fn test_mmc_rsp_bits_simple() {
        let mut resp = [0u32; 4];
        resp[0] = 0x1234_5678;
        // Extract bits 4-7 (the upper nibble of the low byte).
        assert_eq!(mmc_rsp_bits(&resp, 4, 4), 0x7);
    }

    #[test]
    fn test_mmc_rsp_bits_cross_word() {
        let mut resp = [0u32; 4];
        resp[0] = 0xFFFF_FFFF;
        resp[1] = 0x0000_0001;
        // Bit 31 of resp[0] is the MSB; bit 32 is bit 0 of resp[1].
        assert_eq!(mmc_rsp_bits(&resp, 31, 2), 0x3);
    }

    #[test]
    fn test_mmc_rsp_bits_empty() {
        let resp = [0u32; 4];
        assert_eq!(mmc_rsp_bits(&resp, -1, 4), 0);
        assert_eq!(mmc_rsp_bits(&resp, 0, 33), 0);
    }

    #[test]
    fn test_mmc_csd_capacity() {
        let resp = [0u32; 4];
        let cap = mmc_csd_capacity(&resp);
        // All zeros: C_SIZE=0, C_SIZE_MULT=0, cap = (0+1) << (0+2) = 4
        assert_eq!(cap, 4);
    }

    #[test]
    fn test_sd_csd_v2_capacity() {
        let resp = [0u32; 4];
        // Set V2 C_SIZE (22 bits at position 48).
        // For 1 GB: (1945605 + 1) << 10 = 512M sectors... let's test 0.
        // V2 C_SIZE = 0 → capacity = 1 << 10 = 1024 sectors = 512 KB.
        assert_eq!(sd_csd_v2_capacity(&resp), 1024);
    }

    #[test]
    fn test_rca_helpers() {
        assert_eq!(mmc_arg_rca(0x1234), 0x1234_0000);
        let resp = [0x5678_0000, 0, 0, 0];
        assert_eq!(sd_r6_rca(&resp), 0x5678);
    }

    // ── EXT_CSD ────────────────────────────────────────────────────────

    #[test]
    fn test_ext_csd_constants() {
        assert_eq!(EXT_CSD_BUS_WIDTH, 183);
        assert_eq!(EXT_CSD_REV, 192);
        assert_eq!(EXT_CSD_STRUCTURE_VER_1_0, 0);
        assert_eq!(EXT_CSD_CARD_TYPE_26M, 1);
        assert_eq!(EXT_CSD_CARD_TYPE_52M, 2);
    }

    // ── SDHCI Registers ────────────────────────────────────────────────

    #[test]
    fn test_sdhci_register_offsets() {
        assert_eq!(SDHC_DMA_ADDR, 0x00);
        assert_eq!(SDHC_BLOCK_SIZE, 0x04);
        assert_eq!(SDHC_BLOCK_COUNT, 0x06);
        assert_eq!(SDHC_ARGUMENT, 0x08);
        assert_eq!(SDHC_TRANSFER_MODE, 0x0c);
        assert_eq!(SDHC_COMMAND, 0x0e);
        assert_eq!(SDHC_RESPONSE, 0x10);
        assert_eq!(SDHC_DATA, 0x20);
        assert_eq!(SDHC_PRESENT_STATE, 0x24);
        assert_eq!(SDHC_HOST_CTL, 0x28);
        assert_eq!(SDHC_POWER_CTL, 0x29);
        assert_eq!(SDHC_CLOCK_CTL, 0x2c);
        assert_eq!(SDHC_SOFTWARE_RESET, 0x2f);
        assert_eq!(SDHC_NINTR_STATUS, 0x30);
        assert_eq!(SDHC_EINTR_STATUS, 0x32);
        assert_eq!(SDHC_CAPABILITIES, 0x40);
        assert_eq!(SDHC_HOST_VER, 0xFC);
    }

    #[test]
    fn test_sdhci_flags() {
        assert_eq!(SDHC_RESET_ALL, 1);
        assert_eq!(SDHC_CMD_INHIBIT_MASK, 0x0003);
        assert_eq!(SDHC_BLOCK_COUNT_MAX, 512);
        assert_eq!(SDHC_CARD_INSERTED, 1 << 16);
        assert_eq!(SDHC_COMMAND_COMPLETE, 1);
        assert_eq!(SDHC_TRANSFER_COMPLETE, 1 << 1);
        assert_eq!(SDHC_ERROR_INTERRUPT, 1 << 15);
    }

    // ── Types ──────────────────────────────────────────────────────────

    #[test]
    fn test_mmc_command_new() {
        let cmd = MmcCommand::new();
        assert_eq!(cmd.cmd, 0);
        assert_eq!(cmd.args, 0);
        assert!(cmd.data.is_none());
    }

    #[test]
    fn test_sd_card_new() {
        let card = SdCard::new();
        assert_eq!(card.blk_size, 512);
        assert_eq!(card.block_size, 512);
        assert_eq!(card.card_state, CardState::Initial);
    }

    #[test]
    fn test_sd_slot_new() {
        let slot = SdSlot::new(0);
        assert_eq!(slot.host_idx, 0);
        assert_eq!(slot.state, SD_MODE_UNINITIALIZED);
    }

    #[test]
    fn test_card_state_values() {
        assert_eq!(CardState::Initial as u8, 0);
        assert_eq!(CardState::Identified as u8, 1);
        assert_eq!(CardState::Deaf as u8, 2);
        assert_eq!(CardState::Dead as u8, 3);
        assert_eq!(CardState::Disconnected as u8, 4);
    }

    // ── Dummy Host ─────────────────────────────────────────────────────

    #[test]
    fn test_dummy_host_reset() {
        let mut host = DummyHost::new();
        assert!(host.reset().is_ok());
    }

    #[test]
    fn test_dummy_host_card_detect() {
        let host = DummyHost::new();
        let slot = SdSlot::new(0);
        assert!(host.card_detect(&slot));
    }

    #[test]
    fn test_dummy_host_no_card() {
        let host = DummyHost {
            card_present: false,
        };
        let slot = SdSlot::new(0);
        assert!(!host.card_detect(&slot));
    }

    #[test]
    fn test_dummy_host_initialize() {
        let mut host = DummyHost::new();
        let mut slot = SdSlot::new(0);
        assert!(host.card_initialize(&mut slot).is_ok());
        assert_eq!(slot.card.card_state, CardState::Identified);
        assert_eq!(slot.card.blk_size, 512);
        assert_eq!(slot.card.blk_count, 1024 * 1024);
    }

    #[test]
    fn test_dummy_host_release() {
        let mut host = DummyHost::new();
        let mut card = SdCard::new();
        card.card_state = CardState::Identified;
        assert!(host.card_release(&mut card).is_ok());
        assert_eq!(card.card_state, CardState::Disconnected);
    }

    #[test]
    fn test_dummy_host_read() {
        let mut host = DummyHost::new();
        let card = SdCard::new();
        let mut buf = [0xABu8; 512];
        assert!(host.read(&card, 0, 1, &mut buf).is_ok());
        assert_eq!(buf, [0u8; 512]);
    }

    #[test]
    fn test_dummy_host_write() {
        let mut host = DummyHost::new();
        let card = SdCard::new();
        let buf = [0xCDu8; 512];
        assert!(host.write(&card, 0, 1, &buf).is_ok());
    }

    // ── MMC Block Size ─────────────────────────────────────────────────

    #[test]
    fn test_mmc_block_size() {
        assert_eq!(MMC_BLOCK_SIZE, 512);
    }

    // ── Slot / Partition Constants ──────────────────────────────────────

    #[test]
    fn test_slot_constants() {
        assert_eq!(MAX_SD_SLOTS, 4);
        assert_eq!(DEV_PER_DRIVE, 5);
        assert_eq!(SUB_PER_DRIVE, 16);
        assert_eq!(SUBPARTITION_PER_PARTITION, 4);
    }
}
