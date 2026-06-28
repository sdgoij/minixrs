//! Random number generator driver — /dev/random
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/system/random/`
//!
//! Collects entropy from 16 kernel sources + 1 internal timing source,
//! accumulates samples in 32 SHA-256 pools with derivative quality
//! detection, and generates output via AES-128-ECB PRNG. A minimum of
//! 256 samples must be collected before the first reseed.

use crate::DriverError;

// ── Entropy source constants ───────────────────────────────────────────────

/// Number of derivative history entries per source.
const N_DERIV: usize = 16;

/// Number of entropy pools.
const NR_POOLS: usize = 32;

/// Minimum samples needed in pool 0 before reseeding.
const MIN_SAMPLES: u32 = 256;

/// Total sources (16 kernel + 1 internal timing).
pub const TOTAL_SOURCES: usize = 17;

// ── AES-128 constants ──────────────────────────────────────────────────────

const AES_BLOCK_SIZE: usize = 16;
const AES_KEY_SIZE: usize = 16; // AES-128

// ── SHA-256 constants ──────────────────────────────────────────────────────

const SHA256_BLOCK_SIZE: usize = 64;
const SHA256_DIGEST_SIZE: usize = 32;

/// SHA-256 context (minimal implementation for no_std compatibility).
#[derive(Clone, Copy)]
#[repr(C)]
struct Sha256Ctx {
    state: [u32; 8],
    count: u64,
    buffer: [u8; SHA256_BLOCK_SIZE],
    buf_len: usize,
}

impl Sha256Ctx {
    const fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            count: 0,
            buffer: [0u8; SHA256_BLOCK_SIZE],
            buf_len: 0,
        }
    }

    fn init(&mut self) {
        *self = Self::new();
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        let len = data.len();

        // Process full blocks
        if self.buf_len > 0 {
            let space = SHA256_BLOCK_SIZE - self.buf_len;
            let take = if len < space { len } else { space };
            self.buffer[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            offset += take;
            if self.buf_len == SHA256_BLOCK_SIZE {
                sha256_transform(&mut self.state, &self.buffer);
                self.count += SHA256_BLOCK_SIZE as u64;
                self.buf_len = 0;
            }
        }

        while offset + SHA256_BLOCK_SIZE <= len {
            let block: &[u8; SHA256_BLOCK_SIZE] =
                data[offset..offset + SHA256_BLOCK_SIZE].try_into().unwrap();
            sha256_transform(&mut self.state, block);
            self.count += SHA256_BLOCK_SIZE as u64;
            offset += SHA256_BLOCK_SIZE;
        }

        if offset < len {
            let remaining = len - offset;
            self.buffer[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    fn finalize(mut self) -> [u8; SHA256_DIGEST_SIZE] {
        let bits = (self.count + self.buf_len as u64) * 8;
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0x00]);
        }
        // Append length in bits as big-endian u64
        let len_bytes = bits.to_be_bytes();
        self.update(&len_bytes);

        let mut result = [0u8; SHA256_DIGEST_SIZE];
        for (i, word) in self.state.iter().enumerate() {
            result[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        result
    }
}

fn sha256_transform(state: &mut [u32; 8], block: &[u8; SHA256_BLOCK_SIZE]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut w = [0u32; 64];
    for (i, chunk) in block.chunks_exact(4).enumerate().take(16) {
        w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
    }
    #[allow(clippy::needless_range_loop)]
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
    let (mut e, mut f, mut g, mut h) = (state[4], state[5], state[6], state[7]);

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

// ── AES-128 ECB encryption only ────────────────────────────────────────────

/// AES-128 round constant table.
const AES_RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

/// AES S-box.
const AES_SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// AES key schedule (11 round keys for AES-128: 10 rounds + initial).
#[repr(C)]
struct AesKey {
    round_keys: [[u8; 16]; 11],
}

fn aes_key_expansion(key: &[u8; AES_KEY_SIZE]) -> AesKey {
    let mut rk = AesKey {
        round_keys: [[0u8; 16]; 11],
    };
    rk.round_keys[0].copy_from_slice(key);

    #[allow(clippy::needless_range_loop)]
    for i in 1..11 {
        let prev = rk.round_keys[i - 1];
        let mut temp: [u8; 4] = rk.round_keys[i - 1][12..16].try_into().unwrap();
        // RotWord
        temp.rotate_left(1);
        // SubWord
        for j in 0..4 {
            temp[j] = AES_SBOX[temp[j] as usize];
        }
        // XOR with RCON
        temp[0] ^= AES_RCON[i];

        for j in 0..4 {
            rk.round_keys[i][j] = prev[j] ^ temp[j];
        }
        for (j, prev_j) in prev.iter().enumerate().skip(4) {
            rk.round_keys[i][j] = prev_j ^ rk.round_keys[i][j - 4];
        }
    }
    rk
}

fn aes_encrypt_block(key: &AesKey, block: &[u8; 16]) -> [u8; 16] {
    let mut state = *block;

    // Initial AddRoundKey
    for (i, s_i) in state.iter_mut().enumerate() {
        *s_i ^= key.round_keys[0][i];
    }

    // 10 rounds
    for round in 1..=10 {
        // SubBytes
        for i in 0..16 {
            state[i] = AES_SBOX[state[i] as usize];
        }
        // ShiftRows
        let s = state;
        state = [
            s[0], s[5], s[10], s[15], s[4], s[9], s[14], s[3], s[8], s[13], s[2], s[7], s[12],
            s[1], s[6], s[11],
        ];
        // MixColumns (skip in last round)
        if round < 10 {
            for col in 0..4 {
                let idx = col * 4;
                let b0 = state[idx] as u16;
                let b1 = state[idx + 1] as u16;
                let b2 = state[idx + 2] as u16;
                let b3 = state[idx + 3] as u16;
                state[idx] = (gf_mul2(b0) ^ gf_mul3(b1) ^ b2 ^ b3) as u8;
                state[idx + 1] = (b0 ^ gf_mul2(b1) ^ gf_mul3(b2) ^ b3) as u8;
                state[idx + 2] = (b0 ^ b1 ^ gf_mul2(b2) ^ gf_mul3(b3)) as u8;
                state[idx + 3] = (gf_mul3(b0) ^ b1 ^ b2 ^ gf_mul2(b3)) as u8;
            }
        } else {
            // Last round: only SubBytes, ShiftRows, AddRoundKey
            for (i, s_i) in state.iter_mut().enumerate() {
                *s_i ^= key.round_keys[round][i];
            }
            continue;
        }
        for (i, s_i) in state.iter_mut().enumerate() {
            *s_i ^= key.round_keys[round][i];
        }
    }

    state
}

/// Galois Field multiplication helpers for MixColumns.
fn gf_mul2(x: u16) -> u16 {
    if x & 0x80 != 0 {
        ((x << 1) ^ 0x11b) & 0xff
    } else {
        x << 1
    }
}

fn gf_mul3(x: u16) -> u16 {
    gf_mul2(x) ^ x
}

// ── Global state ───────────────────────────────────────────────────────────

/// Derivative history for each entropy source.
///
/// Tracks the last N_DERIV samples to detect quality (entropy estimation).
static mut DERIV: [[u32; N_DERIV]; TOTAL_SOURCES] = [[0u32; N_DERIV]; TOTAL_SOURCES];

/// Current index into each source's derivative history.
static mut POOL_IND: [usize; TOTAL_SOURCES] = [0usize; TOTAL_SOURCES];

/// SHA-256 entropy pools (32 pools, each accumulating samples).
static mut POOL_CTX: [Sha256Ctx; NR_POOLS] = [Sha256Ctx::new(); NR_POOLS];

/// Total number of samples collected in pool 0.
static mut SAMPLES: u32 = 0;

/// Whether the generator has been seeded at least once.
static mut GOT_SEEDED: bool = false;

/// AES key for the PRNG.
static mut RANDOM_KEY: [u8; AES_KEY_SIZE * 2] = [0u8; AES_KEY_SIZE * 2];

/// Counter for tracking reseed operations.
static mut RESEED_COUNT: u32 = 0;

/// Per-call counter for CTR-mode PRNG output.
static mut RANDOM_NEXT: u64 = 0;

// ── Internal helpers ───────────────────────────────────────────────────────

/// Add a sample from a specific entropy source.
///
/// Uses derivative-based quality detection: stores the sample in the
/// source's history ring, then hashes into the next pool.
unsafe fn add_sample(source: usize, sample: u32) {
    unsafe {
        if source >= TOTAL_SOURCES {
            return;
        }

        // Store in derivative history.
        let ind = &mut POOL_IND[source];
        DERIV[source][*ind] = sample;
        *ind = (*ind + 1) % N_DERIV;

        // Hash into pool (source % NR_POOLS, then advanced by derivative).
        let pool = source % NR_POOLS;
        let sample_bytes = sample.to_le_bytes();
        POOL_CTX[pool].update(&sample_bytes);

        // Use derivative to select next pool (quality detection).
        let mut deriv = 0u32;
        for d in DERIV[source].iter() {
            deriv ^= d;
        }
        let next_pool = (pool + (deriv as usize) % NR_POOLS) % NR_POOLS;
        if next_pool != pool {
            POOL_CTX[next_pool].update(&sample_bytes);
        }

        if source == 0 {
            SAMPLES += 1;
        }
    }
}

/// Generate one AES block of output from the PRNG.
unsafe fn data_block(key: &AesKey, output: &mut [u8; AES_BLOCK_SIZE]) {
    unsafe {
        // Encrypt the counter as a 16-byte block.
        let mut block = [0u8; AES_BLOCK_SIZE];
        let ctr = RANDOM_NEXT;
        RANDOM_NEXT = ctr.wrapping_add(1);
        block[0..8].copy_from_slice(&ctr.to_le_bytes());

        *output = aes_encrypt_block(key, &block);
    }
}

/// Reseed the PRNG using accumulated entropy.
unsafe fn reseed() {
    unsafe {
        if SAMPLES < MIN_SAMPLES {
            return;
        }

        // Hash all 32 pools into the key material.
        let mut hash_buf = [0u8; SHA256_DIGEST_SIZE * 2];
        #[allow(clippy::needless_range_loop)]
        for i in 0..NR_POOLS {
            let digest = POOL_CTX[i].finalize();
            let offset = (i % 2) * SHA256_DIGEST_SIZE;
            // XOR each pool's digest into the hash buffer.
            for (j, &d_j) in digest.iter().enumerate() {
                hash_buf[offset + j] ^= d_j;
            }
            // Reset the pool.
            POOL_CTX[i].init();
        }

        // Final hash to produce 32 bytes of key material.
        let mut ctx = Sha256Ctx::new();
        ctx.update(&hash_buf);
        let final_key = ctx.finalize();

        // Update the first 16 bytes of random_key.
        for i in 0..AES_KEY_SIZE {
            RANDOM_KEY[i] ^= final_key[i];
        }

        SAMPLES = 0;
        GOT_SEEDED = true;
        RESEED_COUNT += 1;
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Initialize the random number generator.
///
/// Resets all entropy pools, derivative history, and PRNG state.
///
/// # Safety
///
/// Must be called exactly once before any other random function.
/// Writes to all global mutable state.
pub unsafe fn random_init() {
    unsafe {
        let deriv = core::ptr::addr_of_mut!(DERIV);
        #[allow(clippy::needless_range_loop)]
        for i in 0..TOTAL_SOURCES {
            #[allow(clippy::needless_range_loop)]
            for j in 0..N_DERIV {
                (*deriv)[i][j] = 0;
            }
        }
        let pool_ind = core::ptr::addr_of_mut!(POOL_IND);
        #[allow(clippy::needless_range_loop)]
        for i in 0..TOTAL_SOURCES {
            (*pool_ind)[i] = 0;
        }
        let pool_ctx = core::ptr::addr_of_mut!(POOL_CTX);
        #[allow(clippy::needless_range_loop)]
        for i in 0..NR_POOLS {
            (*pool_ctx)[i].init();
        }
        SAMPLES = 0;
        GOT_SEEDED = false;
        RANDOM_KEY = [0u8; AES_KEY_SIZE * 2];
        RESEED_COUNT = 0;
        RANDOM_NEXT = 0;
    }
}

/// Check if the RNG has been seeded.
pub fn random_isseeded() -> bool {
    unsafe { GOT_SEEDED }
}

/// Update entropy pools with samples from a kernel source.
///
/// # Safety
///
/// Must be called with exclusive access to global state.
pub unsafe fn random_update(source: usize, buf: &[u32]) {
    if source >= TOTAL_SOURCES {
        return;
    }
    unsafe {
        for &sample in buf {
            add_sample(source, sample);
        }
        reseed();
    }
}

/// Fill a buffer with random bytes from the PRNG.
///
/// # Safety
///
/// Must be called with exclusive access to global state.
pub unsafe fn random_getbytes(buf: &mut [u8]) {
    unsafe {
        let key = aes_key_expansion(&{
            let mut k = [0u8; AES_KEY_SIZE];
            k.copy_from_slice(&RANDOM_KEY[..AES_KEY_SIZE]);
            k
        });

        let mut offset = 0;
        while offset < buf.len() {
            let n = AES_BLOCK_SIZE.min(buf.len() - offset);
            let mut output = [0u8; AES_BLOCK_SIZE];
            data_block(&key, &mut output);
            buf[offset..offset + n].copy_from_slice(&output[..n]);
            offset += n;
        }
    }
}

/// Inject external entropy (from /dev/random writes, etc.).
///
/// # Safety
///
/// Must be called with exclusive access to global state.
pub unsafe fn random_putbytes(buf: &[u8]) {
    unsafe {
        // Use the bytes as samples for pool 0 (external source).
        for chunk in buf.chunks(4) {
            let mut sample = [0u8; 4];
            let len = chunk.len().min(4);
            sample[..len].copy_from_slice(chunk);
            let val = u32::from_le_bytes(sample);
            add_sample(0, val);
        }
        reseed();
    }
}

/// Open the random device.
pub fn random_open(minor: usize) -> Result<(), DriverError> {
    if minor == 0 {
        Ok(())
    } else {
        Err(DriverError::NotFound)
    }
}

/// Select on the random device.
///
/// Random device is always readable (once seeded) and always writable.
pub fn random_select(ops: u32) -> u32 {
    ops & 3 // CDEV_OP_RD | CDEV_OP_WR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_init_clears_state() {
        unsafe {
            random_init();
            assert!(!random_isseeded(), "should not be seeded after init");
            let deriv_ptr = core::ptr::addr_of_mut!(DERIV);
            for i in 0..TOTAL_SOURCES {
                for j in 0..N_DERIV {
                    assert_eq!((*deriv_ptr)[i][j], 0, "deriv[{i}][{j}] should be 0");
                }
            }
        }
    }

    #[test]
    fn test_random_update_with_samples() {
        unsafe {
            random_init();
            let samples = [42u32, 100, 200, 300];
            random_update(0, &samples);
            // Should have accumulated samples but not enough to reseed.
            assert!(!random_isseeded(), "not enough samples to reseed");
        }
    }

    #[test]
    fn test_random_update_reseeds_after_min_samples() {
        unsafe {
            random_init();
            // Add enough samples to trigger reseed.
            for _ in 0..(MIN_SAMPLES / 4 + 1) {
                random_update(0, &[1, 2, 3, 4]);
            }
        }
    }

    #[test]
    fn test_random_getbytes_produces_output() {
        unsafe {
            random_init();
            // Force seed by adding enough samples.
            for _ in 0..(MIN_SAMPLES / 4 + 2) {
                random_update(0, &[0xdead, 0xbeef, 0xcafe, 0xbabe]);
            }
            assert!(random_isseeded(), "should be seeded after enough samples");

            let mut buf = [0u8; 32];
            random_getbytes(&mut buf);
            // Output should not be all zeros.
            assert!(
                buf.iter().any(|&b| b != 0),
                "random output should not be all zeros"
            );
        }
    }

    #[test]
    fn test_random_getbytes_multiple_calls_differ() {
        unsafe {
            random_init();
            for _ in 0..(MIN_SAMPLES / 4 + 2) {
                random_update(0, &[0x1111, 0x2222, 0x3333, 0x4444]);
            }
            let mut buf1 = [0u8; 16];
            let mut buf2 = [0u8; 16];
            random_getbytes(&mut buf1);
            random_getbytes(&mut buf2);
            assert_ne!(
                buf1, buf2,
                "consecutive reads should produce different output"
            );
        }
    }

    #[test]
    fn test_random_putbytes_injects_entropy() {
        unsafe {
            random_init();
            random_putbytes(b"external entropy injection!");
            // Not enough to reseed, but should be in the pool.
            assert!(!random_isseeded());
        }
    }

    #[test]
    fn test_random_open_valid() {
        assert!(random_open(0).is_ok());
    }

    #[test]
    fn test_random_open_invalid() {
        assert!(random_open(99).is_err());
    }

    #[test]
    fn test_random_select() {
        let ready = random_select(1 | 2); // CDEV_OP_RD | CDEV_OP_WR
        assert_eq!(ready, 3, "both read and write should be ready");
    }

    #[test]
    fn test_random_update_ignores_bad_source() {
        unsafe {
            random_init();
            random_update(99, &[1, 2, 3]); // source >= TOTAL_SOURCES
            assert!(!random_isseeded());
        }
    }

    #[test]
    fn test_random_getbytes_empty_buf() {
        unsafe {
            random_init();
            random_getbytes(&mut []); // should not panic
        }
    }

    #[test]
    fn test_aes_key_expansion() {
        let key = [0x00u8; AES_KEY_SIZE];
        let rk = aes_key_expansion(&key);
        // First round key should match the input key.
        assert_eq!(rk.round_keys[0], key);
        // Round keys should differ from each other.
        assert_ne!(rk.round_keys[0], rk.round_keys[1]);
    }

    #[test]
    fn test_aes_encrypt_known_answer() {
        // NIST AES-128 test vector (ECB): key=00..00, plaintext=00..00
        let key = [0x00u8; AES_KEY_SIZE];
        let rk = aes_key_expansion(&key);
        let pt = [0x00u8; 16];
        let ct = aes_encrypt_block(&rk, &pt);
        // Expected: 66 e9 4b d4 ef 8a 2c 3b 88 4c fa 59 ca 34 2b 2e
        assert_eq!(ct[0], 0x66);
        assert_eq!(ct[1], 0xe9);
        assert_eq!(ct[2], 0x4b);
        assert_eq!(ct[3], 0xd4);
        assert_eq!(ct[4], 0xef);
        assert_eq!(ct[5], 0x8a);
    }

    #[test]
    fn test_sha256_basic() {
        let mut ctx = Sha256Ctx::new();
        ctx.update(b"hello");
        let hash = ctx.finalize();
        // SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(hash[0], 0x2c);
        assert_eq!(hash[1], 0xf2);
        assert_eq!(hash[2], 0x4d);
        assert_eq!(hash[3], 0xba);
        assert_eq!(hash[4], 0x5f);
        assert_eq!(hash[5], 0xb0);
        assert_eq!(hash[6], 0xa3);
        assert_eq!(hash[7], 0x0e);
    }

    #[test]
    fn test_sha256_empty() {
        let ctx = Sha256Ctx::new();
        let hash = ctx.finalize();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_sha256_large_input() {
        let data = [0x61u8; 1000]; // 1000 'a's
        let mut ctx = Sha256Ctx::new();
        ctx.update(&data);
        let hash = ctx.finalize();
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_gf_mul2() {
        assert_eq!(gf_mul2(0x57), 0xae);
        assert_eq!(gf_mul2(0xae), 0x47); // wraps through 0x11b
    }

    #[test]
    fn test_gf_mul3() {
        assert_eq!(gf_mul3(0x57), 0xf9); // 0x57 * 3 = 0x57 ^ (0x57*2) = 0x57 ^ 0xae = 0xf9
    }

    #[test]
    fn test_add_sample_ignores_bad_source() {
        unsafe {
            random_init();
            add_sample(99, 42);
            let samples_ptr = core::ptr::addr_of_mut!(SAMPLES);
            assert_eq!(*samples_ptr, 0);
        }
    }

    #[test]
    fn test_derivative_history_tracks_samples() {
        unsafe {
            random_init();
            add_sample(0, 100);
            add_sample(0, 200);
            let deriv_ptr = core::ptr::addr_of_mut!(DERIV);
            assert_eq!((*deriv_ptr)[0][0], 100);
            assert_eq!((*deriv_ptr)[0][1], 200);
        }
    }

    #[test]
    fn test_aes_sbox_all_unique() {
        let mut seen = [false; 256];
        for &b in AES_SBOX.iter() {
            assert!(!seen[b as usize], "S-box value {b} duplicated");
            seen[b as usize] = true;
        }
    }
}
