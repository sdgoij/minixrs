//! Storage filter driver — checksumming and mirroring layer over block devices.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/storage/filter/`
//!
//! This driver sits between VFS and an underlying disk driver, providing:
//!
//! - **Checksumming**: CRC32 or MD5 integrity checks on every sector,
//!   stored in interspersed checksum sectors on disk.
//! - **Mirroring**: transparent replication to a second disk.
//! - **Retry logic**: automatic retry and driver restart on failure.
//!
//! ## Architecture
//!
//! The C reference has six files (`main.c`, `driver.c`, `sum.c`, `crc.c`,
//! `md5.c`, `util.c`).  This Rust port consolidates them:
//!
//! - **CRC32 & MD5** — pure computation, fully implemented.
//! - **Checksum math** (`calc_sum_into`, `expand`, `collapse`, `convert`) — pure
//!   data transformations, fully implemented.
//! - **Disk I/O** (`read_write`, `read_sectors`, `transfer`,
//!   `make_sum`, `check_sum`) — deferred; depends on IPC communication with
//!   the underlying disk driver (system server infrastructure, Phase 12).
//! - **Driver lifecycle** (`driver_init`, `driver_shutdown`, `ds_event`,
//!   `check_driver`, `bad_driver`) — deferred; depends on RS, DS, and
//!   blockdriver framework.
//! - **Memory allocation** (`flt_malloc`, `flt_free`) — deferred; depends
//!   on `alloc_contig` / `PhysicalAllocator`.

// Types & Constants

pub const SECTOR_SIZE: usize = 512;

/// Error code for I/O errors.
pub const EIO: i32 = -5;

/// Checksum algorithm types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChecksumType {
    Nil = 0,
    Xor = 1,
    Crc = 2,
    Md5 = 3,
}

/// Disk operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DiskOp {
    Write = 0,
    Read = 1,
    ReadBoth = 2,
}

/// Driver problem state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DriverProblem {
    None = 0,
    Dead = 1,
    Proto = 2,
    Data = 3,
}

/// Return value indicating a request needs to be retried.
pub const RET_REDO: i32 = 1;

/// Driver indices.
pub const DRIVER_MAIN: usize = 0;
pub const DRIVER_BACKUP: usize = 1;

/// Buffer sizes.
pub const BUF_SIZE: usize = 256 * 1024;
pub const SBUF_SIZE: usize = BUF_SIZE * 2;
pub const LABEL_SIZE: usize = 32;

/// Driver info for one underlying disk.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct DriverInfo {
    pub label: [u8; LABEL_SIZE],
    pub minor: i32,
    pub endpoint: i32,
    pub up_event: i32,
    pub problem: DriverProblem,
    pub error: i32,
    pub retries: i32,
    pub kills: i32,
}

impl Default for DriverInfo {
    fn default() -> Self {
        Self {
            label: [0u8; LABEL_SIZE],
            minor: -1,
            endpoint: -1,
            up_event: 0,
            problem: DriverProblem::None,
            error: 0,
            retries: 0,
            kills: 0,
        }
    }
}

// Configuration (mutable globals mirroring the C `extern int` variables)

/// Runtime configuration of the filter driver.
#[repr(C)]
pub struct FilterConfig {
    pub use_checksum: bool,
    pub use_mirror: bool,
    pub bad_sum_error: bool,
    pub use_sum_layout: bool,
    pub nr_sum_sec: u32,
    pub sum_type: ChecksumType,
    pub sum_size: u32,
    pub nr_retries: u32,
    pub nr_restarts: u32,
    pub driver_timeout: u32,
    pub chunk_size: u32,
    pub main_label: [u8; LABEL_SIZE],
    pub backup_label: [u8; LABEL_SIZE],
    pub main_minor: i32,
    pub backup_minor: i32,
}

impl FilterConfig {
    pub const fn default() -> Self {
        Self {
            use_checksum: false,
            use_mirror: false,
            bad_sum_error: true,
            use_sum_layout: false,
            nr_sum_sec: 8,
            sum_type: ChecksumType::Crc,
            sum_size: 0,
            nr_retries: 3,
            nr_restarts: 3,
            driver_timeout: 5,
            chunk_size: 0,
            main_label: [0u8; LABEL_SIZE],
            backup_label: [0u8; LABEL_SIZE],
            main_minor: -1,
            backup_minor: -1,
        }
    }

    /// Determine the checksum size for the chosen checksum type.
    pub fn determine_sum_size(&mut self) {
        self.sum_size = match self.sum_type {
            ChecksumType::Nil => 4,
            ChecksumType::Xor => 16,
            ChecksumType::Crc => 4,
            ChecksumType::Md5 => 16,
        };
    }
}

// CRC32

/// Generate the standard CRC-32 lookup table.
///
/// Uses the polynomial 0xEDB88320 (reflected).
const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0u32;
    while n < 256 {
        let mut c = n;
        let mut k = 0;
        while k < 8 {
            if c & 1 != 0 {
                c = 0xEDB88320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            k += 1;
        }
        table[n as usize] = c;
        n += 1;
    }
    table
}

/// CRC32 table matching the C filter driver's `crctab`.
///
/// Index 0 is a special non-standard value (`0x7fffffff`) used as a
/// zero-substitute.  Indices 1..=256 are the standard CRC-32 table.
/// The `aux` counter in `compute_crc` cycles through all 257 entries.
static CRC_TABLE: [u32; 257] = {
    let std_table = make_crc_table();
    let mut table = [0u32; 257];
    table[0] = 0x7fffffff;
    let mut i = 0;
    while i < 256 {
        table[i + 1] = std_table[i];
        i += 1;
    }
    table
};

/// Compute the filter-driver CRC32 checksum over `data`.
///
/// This uses the non-standard table and zero-handling from the Minix
/// filter driver's `crc.c`.  When the computed table index is 0, a
/// secondary counter `aux` substitutes the next entry.
pub fn compute_crc(data: &[u8]) -> u32 {
    let mut s: u32 = 0;
    let mut aux: usize = 0;
    for &b in data {
        let idx = ((s >> 24) ^ (b as u32)) as usize;
        // When idx == 0, the C code substitutes the next value from a
        // cycling counter that covers all 257 table entries.
        let i = if idx == 0 {
            let v = aux;
            aux = (aux + 1) % 257;
            v
        } else {
            idx
        };
        s = (s << 8) ^ CRC_TABLE[i];
    }
    s
}

// MD5

/// MD5 message-digest context.
#[derive(Clone)]
#[repr(C)]
pub struct MD5Context {
    buf: [u32; 4],
    bits: [u32; 2],
    input: [u8; 64],
}

macro_rules! md5_step {
    ($a:ident, $b:ident, $c:ident, $d:ident, $xk:expr, $s:expr, $t:expr) => {
        $a = $a
            .wrapping_add($d ^ ($b & ($c ^ $d)))
            .wrapping_add($xk)
            .wrapping_add($t);
        $a = $a.rotate_left($s).wrapping_add($b);
    };
}

macro_rules! md5_step2 {
    ($a:ident, $b:ident, $c:ident, $d:ident, $xk:expr, $s:expr, $t:expr) => {
        $a = $a
            .wrapping_add($c ^ ($d & ($b ^ $c)))
            .wrapping_add($xk)
            .wrapping_add($t);
        $a = $a.rotate_left($s).wrapping_add($b);
    };
}

macro_rules! md5_step3 {
    ($a:ident, $b:ident, $c:ident, $d:ident, $xk:expr, $s:expr, $t:expr) => {
        $a = $a
            .wrapping_add($b ^ $c ^ $d)
            .wrapping_add($xk)
            .wrapping_add($t);
        $a = $a.rotate_left($s).wrapping_add($b);
    };
}

macro_rules! md5_step4 {
    ($a:ident, $b:ident, $c:ident, $d:ident, $xk:expr, $s:expr, $t:expr) => {
        $a = $a
            .wrapping_add($c ^ ($b | !$d))
            .wrapping_add($xk)
            .wrapping_add($t);
        $a = $a.rotate_left($s).wrapping_add($b);
    };
}

impl Default for MD5Context {
    fn default() -> Self {
        Self::new()
    }
}

impl MD5Context {
    pub fn new() -> Self {
        Self {
            buf: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            bits: [0, 0],
            input: [0u8; 64],
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        let t = self.bits[0];
        self.bits[0] = t.wrapping_add((data.len() as u32) << 3);
        if self.bits[0] < t {
            self.bits[1] = self.bits[1].wrapping_add(1);
        }
        self.bits[1] = self.bits[1].wrapping_add((data.len() as u32) >> 29);

        let t2 = (t >> 3) & 0x3f;

        if t2 != 0 {
            let space = 64 - t2;
            if (data.len() as u32) < space {
                let start = t2 as usize;
                self.input[start..start + data.len()].copy_from_slice(data);
                return;
            }
            let start = t2 as usize;
            let end = start + space as usize;
            self.input[start..end].copy_from_slice(&data[..space as usize]);
            Self::transform(&mut self.buf, &self.input);
            let mut pos = space as usize;
            while pos + 64 <= data.len() {
                Self::transform(&mut self.buf, &data[pos..pos + 64]);
                pos += 64;
            }
            self.input[..data.len() - pos].copy_from_slice(&data[pos..]);
        } else {
            let mut pos = 0;
            while pos + 64 <= data.len() {
                Self::transform(&mut self.buf, &data[pos..pos + 64]);
                pos += 64;
            }
            self.input[..data.len() - pos].copy_from_slice(&data[pos..]);
        }
    }

    pub fn finalize(&mut self) -> [u8; 16] {
        let count = (self.bits[0] >> 3) & 0x3f;
        self.input[count as usize] = 0x80;

        let pad_bytes = 64 - 1 - count;
        if pad_bytes < 8 {
            let start = (count + 1) as usize;
            for i in start..64 {
                self.input[i] = 0;
            }
            Self::transform(&mut self.buf, &self.input);
            self.input[..56].fill(0);
        } else {
            let start = (count + 1) as usize;
            let end = start + (pad_bytes as usize).saturating_sub(8);
            for i in start..end.min(64) {
                self.input[i] = 0;
            }
        }

        let lo = self.bits[0];
        let hi = self.bits[1];
        self.input[56] = lo as u8;
        self.input[57] = (lo >> 8) as u8;
        self.input[58] = (lo >> 16) as u8;
        self.input[59] = (lo >> 24) as u8;
        self.input[60] = hi as u8;
        self.input[61] = (hi >> 8) as u8;
        self.input[62] = (hi >> 16) as u8;
        self.input[63] = (hi >> 24) as u8;

        Self::transform(&mut self.buf, &self.input);

        let mut digest = [0u8; 16];
        for i in 0..4 {
            let v = self.buf[i];
            digest[i * 4] = v as u8;
            digest[i * 4 + 1] = (v >> 8) as u8;
            digest[i * 4 + 2] = (v >> 16) as u8;
            digest[i * 4 + 3] = (v >> 24) as u8;
        }
        *self = Self::new();
        digest
    }

    #[allow(clippy::many_single_char_names, clippy::needless_range_loop)]
    fn transform(buf: &mut [u32; 4], block: &[u8]) {
        let mut a = buf[0];
        let mut b = buf[1];
        let mut c = buf[2];
        let mut d = buf[3];

        let mut x = [0u32; 16];
        for i in 0..16 {
            let off = i * 4;
            x[i] = u32::from_le_bytes([block[off], block[off + 1], block[off + 2], block[off + 3]]);
        }

        md5_step!(a, b, c, d, x[0], 7, 0xd76aa478);
        md5_step!(d, a, b, c, x[1], 12, 0xe8c7b756);
        md5_step!(c, d, a, b, x[2], 17, 0x242070db);
        md5_step!(b, c, d, a, x[3], 22, 0xc1bdceee);
        md5_step!(a, b, c, d, x[4], 7, 0xf57c0faf);
        md5_step!(d, a, b, c, x[5], 12, 0x4787c62a);
        md5_step!(c, d, a, b, x[6], 17, 0xa8304613);
        md5_step!(b, c, d, a, x[7], 22, 0xfd469501);
        md5_step!(a, b, c, d, x[8], 7, 0x698098d8);
        md5_step!(d, a, b, c, x[9], 12, 0x8b44f7af);
        md5_step!(c, d, a, b, x[10], 17, 0xffff5bb1);
        md5_step!(b, c, d, a, x[11], 22, 0x895cd7be);
        md5_step!(a, b, c, d, x[12], 7, 0x6b901122);
        md5_step!(d, a, b, c, x[13], 12, 0xfd987193);
        md5_step!(c, d, a, b, x[14], 17, 0xa679438e);
        md5_step!(b, c, d, a, x[15], 22, 0x49b40821);

        md5_step2!(a, b, c, d, x[1], 5, 0xf61e2562);
        md5_step2!(d, a, b, c, x[6], 9, 0xc040b340);
        md5_step2!(c, d, a, b, x[11], 14, 0x265e5a51);
        md5_step2!(b, c, d, a, x[0], 20, 0xe9b6c7aa);
        md5_step2!(a, b, c, d, x[5], 5, 0xd62f105d);
        md5_step2!(d, a, b, c, x[10], 9, 0x02441453);
        md5_step2!(c, d, a, b, x[15], 14, 0xd8a1e681);
        md5_step2!(b, c, d, a, x[4], 20, 0xe7d3fbc8);
        md5_step2!(a, b, c, d, x[9], 5, 0x21e1cde6);
        md5_step2!(d, a, b, c, x[14], 9, 0xc33707d6);
        md5_step2!(c, d, a, b, x[3], 14, 0xf4d50d87);
        md5_step2!(b, c, d, a, x[8], 20, 0x455a14ed);
        md5_step2!(a, b, c, d, x[13], 5, 0xa9e3e905);
        md5_step2!(d, a, b, c, x[2], 9, 0xfcefa3f8);
        md5_step2!(c, d, a, b, x[7], 14, 0x676f02d9);
        md5_step2!(b, c, d, a, x[12], 20, 0x8d2a4c8a);

        md5_step3!(a, b, c, d, x[5], 4, 0xfffa3942);
        md5_step3!(d, a, b, c, x[8], 11, 0x8771f681);
        md5_step3!(c, d, a, b, x[11], 16, 0x6d9d6122);
        md5_step3!(b, c, d, a, x[14], 23, 0xfde5380c);
        md5_step3!(a, b, c, d, x[1], 4, 0xa4beea44);
        md5_step3!(d, a, b, c, x[4], 11, 0x4bdecfa9);
        md5_step3!(c, d, a, b, x[7], 16, 0xf6bb4b60);
        md5_step3!(b, c, d, a, x[10], 23, 0xbebfbc70);
        md5_step3!(a, b, c, d, x[13], 4, 0x289b7ec6);
        md5_step3!(d, a, b, c, x[0], 11, 0xeaa127fa);
        md5_step3!(c, d, a, b, x[3], 16, 0xd4ef3085);
        md5_step3!(b, c, d, a, x[6], 23, 0x04881d05);
        md5_step3!(a, b, c, d, x[9], 4, 0xd9d4d039);
        md5_step3!(d, a, b, c, x[12], 11, 0xe6db99e5);
        md5_step3!(c, d, a, b, x[15], 16, 0x1fa27cf8);
        md5_step3!(b, c, d, a, x[2], 23, 0xc4ac5665);

        md5_step4!(a, b, c, d, x[0], 6, 0xf4292244);
        md5_step4!(d, a, b, c, x[7], 10, 0x432aff97);
        md5_step4!(c, d, a, b, x[14], 15, 0xab9423a7);
        md5_step4!(b, c, d, a, x[5], 21, 0xfc93a039);
        md5_step4!(a, b, c, d, x[12], 6, 0x655b59c3);
        md5_step4!(d, a, b, c, x[3], 10, 0x8f0ccc92);
        md5_step4!(c, d, a, b, x[10], 15, 0xffeff47d);
        md5_step4!(b, c, d, a, x[1], 21, 0x85845dd1);
        md5_step4!(a, b, c, d, x[8], 6, 0x6fa87e4f);
        md5_step4!(d, a, b, c, x[15], 10, 0xfe2ce6e0);
        md5_step4!(c, d, a, b, x[6], 15, 0xa3014314);
        md5_step4!(b, c, d, a, x[13], 21, 0x4e0811a1);
        md5_step4!(a, b, c, d, x[4], 6, 0xf7537e82);
        md5_step4!(d, a, b, c, x[11], 10, 0xbd3af235);
        md5_step4!(c, d, a, b, x[2], 15, 0x2ad7d2bb);
        md5_step4!(b, c, d, a, x[9], 21, 0xeb86d391);

        buf[0] = buf[0].wrapping_add(a);
        buf[1] = buf[1].wrapping_add(b);
        buf[2] = buf[2].wrapping_add(c);
        buf[3] = buf[3].wrapping_add(d);
    }
}

// Checksum computation (calc_sum in C)

/// Compute the checksum for a single sector's data.
///
/// Writes the result into `sum` (which must be at least `SUM_SIZE` bytes).
pub fn calc_sum_into(sector: u32, data: &[u8], sum: &mut [u8], config: &FilterConfig) {
    match config.sum_type {
        ChecksumType::Nil => {
            let v = sector.to_le_bytes();
            let len = sum.len().min(4);
            sum[..len].copy_from_slice(&v[..len]);
        }
        ChecksumType::Xor => {
            let mut accum = [0u8; 4];
            for chunk in data.chunks(4) {
                for (j, &b) in chunk.iter().enumerate() {
                    accum[j] ^= b;
                }
            }
            let sv = sector.to_le_bytes();
            for j in 0..4 {
                accum[j] ^= sv[j];
            }
            let len = sum.len().min(4);
            sum[..len].copy_from_slice(&accum[..len]);
        }
        ChecksumType::Crc => {
            let crc = compute_crc(data);
            let v = crc ^ sector;
            let bytes = v.to_le_bytes();
            let len = sum.len().min(4);
            sum[..len].copy_from_slice(&bytes[..len]);
        }
        ChecksumType::Md5 => {
            let mut ctx = MD5Context::new();
            ctx.update(data);
            ctx.update(&sector.to_le_bytes());
            let digest = ctx.finalize();
            let len = sum.len().min(16);
            sum[..len].copy_from_slice(&digest[..len]);
        }
    }
}

// Layout math

/// Convert logical sector number to physical sector number (with
/// interspersed checksums).  Corresponds to C `LOG2PHYS`.
pub fn log2phys(sector: u64, nr_sum_sec: u32) -> u64 {
    let nss = nr_sum_sec as u64;
    (sector / nss) * (nss + 1) + (sector % nss)
}

/// Compute the checksum sector number for a given logical sector.
/// Corresponds to C `SEC2SUM_NR`.
pub fn sec2sum_nr(sector: u64, nr_sum_sec: u32) -> u64 {
    let nss = nr_sum_sec as u64;
    (sector / nss) * (nss + 1) + nss
}

/// Expand contiguous user data into interspersed format in `ext_buffer`.
pub fn expand(first_sector: u64, buffer: &[u8], ext_buffer: &mut [u8], nr_sum_sec: u32) {
    let nss = nr_sum_sec as u64;
    let sector_in_group = first_sector % nss;
    let mut src_off = 0usize;
    let mut dst_off = 0usize;
    let mut sectors_left = (buffer.len() / SECTOR_SIZE) as u64;
    let mut group_left = nss - sector_in_group;

    while sectors_left > 0 {
        let count = sectors_left.min(group_left);
        let size = (count * SECTOR_SIZE as u64) as usize;

        let end = dst_off + size;
        if end <= ext_buffer.len() {
            ext_buffer[dst_off..end].copy_from_slice(&buffer[src_off..src_off + size]);
        }

        src_off += size;
        dst_off += size + SECTOR_SIZE;
        sectors_left -= count;
        group_left = nss;
    }
}

/// Collapse interspersed data in `ext_buffer` to contiguous format in
/// `buffer`.  `sizep` is both input (size of ext data to process) and
/// output (resulting contiguous data size).  Returns the output size.
pub fn collapse(
    first_sector: u64,
    ext_buffer: &[u8],
    buffer: &mut [u8],
    size: &mut usize,
    nr_sum_sec: u32,
) {
    let nss = nr_sum_sec as u64;
    let sector_in_group = first_sector % nss;
    let mut src_off = 0usize;
    let mut dst_off = 0usize;
    let mut bytes_left = *size;
    let mut group_bytes_left = ((nss - sector_in_group) * SECTOR_SIZE as u64) as usize;

    while bytes_left > 0 {
        let sz = bytes_left.min(group_bytes_left);
        let end = dst_off + sz;
        if end <= buffer.len() && src_off + sz <= ext_buffer.len() {
            buffer[dst_off..end].copy_from_slice(&ext_buffer[src_off..src_off + sz]);
        }

        let step = sz + SECTOR_SIZE;
        src_off += step;
        dst_off += sz;
        bytes_left = bytes_left.saturating_sub(step);
        group_bytes_left = (nss * SECTOR_SIZE as u64) as usize;
    }

    *size = dst_off;
}

/// Compute sizes for checksum-layout I/O.
///
/// Returns `(req_size, ext_size)` where:
/// - `req_size` = size of user data after expansion (interspersed csum sectors)
/// - `ext_size` = total size including trailing checksum sectors
pub fn expand_sizes(first_sector: u64, nr_sectors: u64, nr_sum_sec: u32) -> (usize, usize) {
    let last_logical = first_sector + nr_sectors - 1;
    let last_phys = log2phys(last_logical, nr_sum_sec);
    let sum_sector = sec2sum_nr(last_logical, nr_sum_sec);
    let first_phys = log2phys(first_sector, nr_sum_sec);

    let req_size = ((last_phys - first_phys + 1) * SECTOR_SIZE as u64) as usize;
    let ext_size = ((sum_sector - first_phys + 1) * SECTOR_SIZE as u64) as usize;
    (req_size, ext_size)
}

/// Compute user-visible size after collapsing checksum layout.
pub fn collapse_size(first_sector: u64, raw_size: usize, nr_sum_sec: u32) -> usize {
    let nss = nr_sum_sec as u64;
    let sector_in_group = first_sector % nss;
    let sectors_from_base = (raw_size / SECTOR_SIZE) as u64 + sector_in_group;
    let sum_secs = sectors_from_base / (nss + 1);
    let nr_data_secs = sectors_from_base - sector_in_group - sum_secs;
    (nr_data_secs * SECTOR_SIZE as u64) as usize
}

/// Given a raw disk size, subtract the space used for checksums.
pub fn convert(raw_size: u64, use_sum_layout: bool, nr_sum_sec: u32) -> u64 {
    if !use_sum_layout {
        return raw_size;
    }
    let nss = nr_sum_sec as u64;
    let sectors = raw_size / (SECTOR_SIZE as u64);
    (sectors / (nss + 1)) * nss * (SECTOR_SIZE as u64)
}

// Checksum group operations (depend on read_write IPC)

/// Callback type for low-level block I/O.
/// Reads or writes `size` bytes at `pos` (physical sector address).
/// Returns the number of bytes transferred on success.
pub type BlockIoFn = unsafe fn(pos: u64, buf: &mut [u8], write: bool) -> Result<usize, i32>;

/// Read sectors from the underlying device via the I/O callback.
unsafe fn read_sectors(io: BlockIoFn, pos: u64, buf: &mut [u8]) -> Result<usize, i32> {
    // SAFETY: io is provided by the caller who guarantees endpoint validity.
    unsafe { io(pos, buf, false) }
}

/// Compute checksums and write them to the underlying device.
unsafe fn make_sum(
    io: BlockIoFn,
    data_buf: &[u8],
    sum_buf: &mut [u8],
    first_sector: u64,
    nr_sectors: u64,
    config: &FilterConfig,
) -> Result<(), i32> {
    for i in 0..nr_sectors {
        let sector = first_sector + i;
        let sum_sector = sec2sum_nr(sector, config.nr_sum_sec);
        let data_off = (i * SECTOR_SIZE as u64) as usize;
        let sum_data = &mut sum_buf[..SECTOR_SIZE];
        sum_data.fill(0);
        calc_sum_into(
            sector as u32,
            &data_buf[data_off..data_off + SECTOR_SIZE],
            sum_data,
            config,
        );
        // Write the checksum sector.
        let phys = sum_sector * SECTOR_SIZE as u64;
        unsafe {
            io(phys, sum_data, true)?;
        }
    }
    Ok(())
}

/// Verify checksums after reading data.
unsafe fn check_sum(
    io: BlockIoFn,
    pos: u64,
    data_buf: &[u8],
    sum_buf: &mut [u8],
    first_sector: u64,
    nr_sectors: u64,
    config: &FilterConfig,
) -> Result<(), i32> {
    let _ = pos;
    for i in 0..nr_sectors {
        let sector = first_sector + i;
        let sum_sector = sec2sum_nr(sector, config.nr_sum_sec);
        let data_off = (i * SECTOR_SIZE as u64) as usize;
        let sum_data = &mut sum_buf[..SECTOR_SIZE];
        sum_data.fill(0);

        // Read the checksum sector.
        let phys = sum_sector * SECTOR_SIZE as u64;
        unsafe {
            io(phys, sum_data, false)?;
        }

        // Compute expected checksum and verify.
        let mut expected = [0u8; SECTOR_SIZE];
        calc_sum_into(
            sector as u32,
            &data_buf[data_off..data_off + SECTOR_SIZE],
            &mut expected,
            config,
        );
        if sum_data[..config.sum_size as usize] != expected[..config.sum_size as usize] {
            return Err(EIO);
        }
    }
    Ok(())
}

/// Full transfer with checksumming (read or write).
///
/// `io` is the callback for low-level sector I/O to the underlying device.
/// `ext_buf` is a scratch buffer for expanded physical I/O (at least `req_size` bytes,
/// where `req_size` is from `expand_sizes`).
/// `sum_buf` is a scratch buffer for checksum data (at least `SECTOR_SIZE` bytes).
pub fn filter_transfer(
    pos: u64,
    buffer: &mut [u8],
    write: bool,
    config: &FilterConfig,
    io: BlockIoFn,
    ext_buf: &mut [u8],
    sum_buf: &mut [u8],
) -> Result<usize, i32> {
    let nr_sectors = (buffer.len() / SECTOR_SIZE) as u64;
    if nr_sectors == 0 {
        return Ok(0);
    }
    let first_sector = pos / SECTOR_SIZE as u64;
    let phys_first = log2phys(first_sector, config.nr_sum_sec) * SECTOR_SIZE as u64;

    unsafe {
        // Read the physical sectors (data + checksums).
        read_sectors(io, phys_first, ext_buf)?;

        if write {
            // Expand the logical buffer into the physical layout.
            expand(first_sector, buffer, ext_buf, config.nr_sum_sec);

            // Write the physical sectors (data + checksums).
            let written = io(phys_first, ext_buf, true)?;

            // Compute and write checksum sectors.
            make_sum(io, ext_buf, sum_buf, first_sector, nr_sectors, config)?;

            Ok(written)
        } else {
            // Collapse the physical data back to logical layout.
            let mut out_size = buffer.len();
            collapse(
                first_sector,
                ext_buf,
                buffer,
                &mut out_size,
                config.nr_sum_sec,
            );

            // Verify checksums.
            check_sum(
                io,
                phys_first,
                ext_buf,
                sum_buf,
                first_sector,
                nr_sectors,
                config,
            )?;

            Ok(out_size)
        }
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn test_constants() {
        assert_eq!(SECTOR_SIZE, 512);
        assert_eq!(BUF_SIZE, 262144);
        assert_eq!(RET_REDO, 1);
        assert_eq!(DRIVER_MAIN, 0);
        assert_eq!(DRIVER_BACKUP, 1);
    }


    #[test]
    fn test_config_default() {
        let cfg = FilterConfig::default();
        assert!(!cfg.use_checksum);
        assert!(!cfg.use_mirror);
        assert!(cfg.bad_sum_error);
        assert_eq!(cfg.nr_sum_sec, 8);
        assert_eq!(cfg.sum_type, ChecksumType::Crc);
        assert_eq!(cfg.nr_retries, 3);
        assert_eq!(cfg.main_minor, -1);
    }

    #[test]
    fn test_determine_sum_size() {
        let mut cfg = FilterConfig::default();
        cfg.sum_type = ChecksumType::Nil;
        cfg.determine_sum_size();
        assert_eq!(cfg.sum_size, 4);

        cfg.sum_type = ChecksumType::Crc;
        cfg.determine_sum_size();
        assert_eq!(cfg.sum_size, 4);

        cfg.sum_type = ChecksumType::Md5;
        cfg.determine_sum_size();
        assert_eq!(cfg.sum_size, 16);

        cfg.sum_type = ChecksumType::Xor;
        cfg.determine_sum_size();
        assert_eq!(cfg.sum_size, 16);
    }


    #[test]
    fn test_checksum_type_repr() {
        assert_eq!(ChecksumType::Nil as u8, 0);
        assert_eq!(ChecksumType::Xor as u8, 1);
        assert_eq!(ChecksumType::Crc as u8, 2);
        assert_eq!(ChecksumType::Md5 as u8, 3);
    }

    #[test]
    fn test_driver_info_default() {
        let di = DriverInfo::default();
        assert_eq!(di.minor, -1);
        assert_eq!(di.endpoint, -1);
        assert_eq!(di.problem, DriverProblem::None);
    }


    #[test]
    fn test_crc32_empty() {
        assert_eq!(compute_crc(b""), 0);
    }

    #[test]
    fn test_crc32_hello() {
        let r = compute_crc(b"hello");
        assert_ne!(r, 0);
    }

    #[test]
    fn test_crc32_short_differs() {
        assert_ne!(compute_crc(b"a"), compute_crc(b"b"));
    }

    #[test]
    fn test_crc32_deterministic() {
        assert_eq!(compute_crc(b"test data"), compute_crc(b"test data"));
    }


    #[test]
    fn test_md5_rfc1321_empty() {
        let digest = MD5Context::new().finalize();
        assert_eq!(
            digest,
            [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e,
            ]
        );
    }

    #[test]
    fn test_md5_rfc1321_a() {
        let mut ctx = MD5Context::new();
        ctx.update(b"a");
        assert_eq!(
            ctx.finalize(),
            [
                0x0c, 0xc1, 0x75, 0xb9, 0xc0, 0xf1, 0xb6, 0xa8, 0x31, 0xc3, 0x99, 0xe2, 0x69, 0x77,
                0x26, 0x61,
            ]
        );
    }

    #[test]
    fn test_md5_rfc1321_abc() {
        let mut ctx = MD5Context::new();
        ctx.update(b"abc");
        assert_eq!(
            ctx.finalize(),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72,
            ]
        );
    }

    #[test]
    fn test_md5_message_digest() {
        let mut ctx = MD5Context::new();
        ctx.update(b"message digest");
        assert_eq!(
            ctx.finalize(),
            [
                0xf9, 0x6b, 0x69, 0x7d, 0x7c, 0xb7, 0x93, 0x8d, 0x52, 0x5a, 0x2f, 0x31, 0xaa, 0xf1,
                0x61, 0xd0,
            ]
        );
    }

    #[test]
    fn test_md5_update_twice() {
        let mut ctx1 = MD5Context::new();
        ctx1.update(b"hello ");
        ctx1.update(b"world");
        let mut ctx2 = MD5Context::new();
        ctx2.update(b"hello world");
        assert_eq!(ctx1.finalize(), ctx2.finalize());
    }

    #[test]
    fn test_md5_64bytes() {
        // Exactly 64 bytes (one block) should not panic.
        let mut ctx = MD5Context::new();
        ctx.update(&[0xABu8; 64]);
        let d = ctx.finalize();
        assert_ne!(d, [0u8; 16]);
    }

    #[test]
    fn test_md5_65bytes() {
        // 65 bytes (one block + 1 byte overflow) should not panic.
        let mut ctx = MD5Context::new();
        ctx.update(&[0xCDu8; 65]);
        let d = ctx.finalize();
        assert_ne!(d, [0u8; 16]);
    }


    #[test]
    fn test_calc_sum_nil() {
        let mut cfg = FilterConfig::default();
        cfg.sum_type = ChecksumType::Nil;
        cfg.determine_sum_size();
        let mut sum = [0u8; 16];
        calc_sum_into(42, &[0u8; 512], &mut sum[..cfg.sum_size as usize], &cfg);
        assert_eq!(sum[..4], 42u32.to_le_bytes());
    }

    #[test]
    fn test_calc_sum_crc() {
        let mut cfg = FilterConfig::default();
        cfg.sum_type = ChecksumType::Crc;
        cfg.determine_sum_size();
        let mut sum = [0u8; 16];
        calc_sum_into(0, &[0u8; 512], &mut sum[..cfg.sum_size as usize], &cfg);
        let crc = compute_crc(&[0u8; 512]);
        assert_eq!(sum[..4], crc.to_le_bytes());
    }

    #[test]
    fn test_calc_sum_md5() {
        let mut cfg = FilterConfig::default();
        cfg.sum_type = ChecksumType::Md5;
        cfg.determine_sum_size();
        let mut sum = [0u8; 16];
        calc_sum_into(1, &[0u8; 512], &mut sum[..cfg.sum_size as usize], &cfg);
        assert_ne!(sum, [0u8; 16]);
    }


    #[test]
    fn test_log2phys_basic() {
        assert_eq!(log2phys(0, 8), 0);
        assert_eq!(log2phys(7, 8), 7);
        assert_eq!(log2phys(8, 8), 9);
        assert_eq!(log2phys(9, 8), 10);
    }

    #[test]
    fn test_sec2sum_nr() {
        assert_eq!(sec2sum_nr(7, 8), 8);
        assert_eq!(sec2sum_nr(15, 8), 17);
    }

    #[test]
    fn test_expand_small() {
        // 3 data sectors at start of group needs ext buffer with
        // gap to checksum sector: sectors 0-2 data, 3-7 gap, 8 csum.
        let mut buf = [0u8; 512 * 3];
        buf[0] = 1;
        buf[512] = 2;
        buf[1024] = 3;
        let mut ext = [0u8; 512 * 9]; // 3 data + 5 gap + 1 csum
        expand(0, &buf, &mut ext, 8);
        assert_eq!(ext[0], 1);
        assert_eq!(ext[512], 2);
        assert_eq!(ext[1024], 3);
    }

    #[test]
    fn test_collapse_small() {
        // ext has 3 data sectors + gap + checksum = 9 sectors.
        let mut ext = [0u8; 512 * 9];
        ext[0] = 1;
        ext[512] = 2;
        ext[1024] = 3;
        let mut buf = [0u8; 512 * 3];
        // C code: *sizep = MIN(req_size, res_size) = MIN(3*512, 9*512) = 1536
        let mut size = 3 * 512;
        collapse(0, &ext, &mut buf, &mut size, 8);
        assert_eq!(size, 512 * 3);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[512], 2);
        assert_eq!(buf[1024], 3);
    }

    #[test]
    fn test_expand_sizes() {
        // 8 data sectors fills one group: req = 8 data sectors, ext = +1 csum.
        let (req, ext) = expand_sizes(0, 8, 8);
        assert_eq!(req, 8 * 512);
        assert_eq!(ext, 9 * 512);
    }

    #[test]
    fn test_expand_sizes_partial() {
        // 3 data sectors. req = 3 * 512 (data only).
        // ext = 9 * 512 (data + gap + trailing csum).
        let (req, ext) = expand_sizes(0, 3, 8);
        assert_eq!(req, 3 * 512);
        assert_eq!(ext, 9 * 512);
    }

    #[test]
    fn test_convert_without_layout() {
        assert_eq!(convert(10000, false, 8), 10000);
    }

    #[test]
    fn test_convert_with_layout() {
        let raw = 9u64 * 512;
        assert_eq!(convert(raw, true, 8), 8 * 512);
    }

    #[test]
    fn test_collapse_size_basic() {
        assert_eq!(collapse_size(0, 9 * 512, 8), 8 * 512);
    }

    #[test]
    fn test_driver_problem_repr() {
        assert_eq!(DriverProblem::None as u8, 0);
        assert_eq!(DriverProblem::Data as u8, 3);
    }

    #[test]
    fn test_disk_op_repr() {
        assert_eq!(DiskOp::Write as u8, 0);
        assert_eq!(DiskOp::Read as u8, 1);
        assert_eq!(DiskOp::ReadBoth as u8, 2);
    }

    #[test]
    fn test_make_crc_table_size() {
        let _t: [u32; 256] = make_crc_table();
    }

    #[test]
    fn test_crc_table_size() {
        assert_eq!(CRC_TABLE.len(), 257);
        assert_eq!(CRC_TABLE[0], 0x7fffffff);
    }
}
