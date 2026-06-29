//! Pseudo-terminal (PTY) driver — bidirectional pipe with TTY processing.
//!
//! Ported from `.refs/minix-3.3.0/minix/drivers/tty/pty/pty.c`
//!
//! A PTY is a bidirectional pipe where one end is the **master**
//! (`/dev/ptypX`, used by network daemons) and the other is the **slave**
//! (`/dev/ttypX`, connected to a shell or program via the TTY layer).
//!
//! Data flow:
//!
//! ```text
//! Master write ──→ in_process() ──→ TTY input queue ──→ Slave read
//!                                                           │
//! Slave write ──→ out_process() ──→ output buffer ──→ Master read
//! ```
//!
//! The PTY manages the output buffer and master-side state.  The TTY
//! server provides `in_process`/`out_process`/`sigchar`/`handle_events`
//! via the [`PtyHost`] trait.

use crate::DriverError;
use core::cell::UnsafeCell;

// ═══════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════

/// Number of PTY pairs.
pub const NR_PTYS: usize = 4;

/// First minor for PTY master devices (/dev/ptypX).
pub const PTYPX_MINOR: u32 = 192;

/// First minor for PTY slave devices (/dev/ttypX).
pub const TTYPX_MINOR: u32 = 128;

/// Output buffer size (bytes from slave→master).
pub const TTY_OUT_BYTES: usize = 2048;

// ── State flags ──────────────────────────────────────────────────────────

/// TTY side is open/active.
const TTY_ACTIVE: u8 = 0x01;
/// PTY (master) side is open/active.
const PTY_ACTIVE: u8 = 0x02;
/// TTY side has closed down.
const TTY_CLOSED: u8 = 0x04;
/// PTY (master) side has closed down.
const PTY_CLOSED: u8 = 0x08;

// ═══════════════════════════════════════════════════════════════════════════
// Host trait — callbacks into the TTY server
// ═══════════════════════════════════════════════════════════════════════════

/// Callbacks that the PTY driver calls into the TTY server for input/output
/// processing, signal delivery, and event handling.
pub trait PtyHost {
    /// Process input bytes from the master writer into the TTY input queue.
    /// Returns the number of bytes consumed (may be less than `data.len()`
    /// if the input queue is full).
    fn in_process(&mut self, data: &[u8]) -> usize;

    /// Process output bytes destined for the master reader.
    /// `len` is updated with the number of bytes consumed from the source,
    /// `ocount` with the number of bytes actually placed in the buffer
    /// (after output processing like tab expansion).
    fn out_process(&mut self, buf: &[u8], len: &mut usize, ocount: &mut usize);

    /// Send a signal to the foreground process group of this TTY.
    fn sigchar(&mut self, sig: u8, may_flush: bool);

    /// Handle TTY events (wake readers/writers, check timers, etc.).
    fn handle_events(&mut self);
}

/// No-op host implementation for use before the TTY server is wired.
pub struct NoopHost;

impl PtyHost for NoopHost {
    fn in_process(&mut self, data: &[u8]) -> usize {
        data.len() // consume everything, discard
    }
    fn out_process(&mut self, _buf: &[u8], _len: &mut usize, _ocount: &mut usize) {}
    fn sigchar(&mut self, _sig: u8, _may_flush: bool) {}
    fn handle_events(&mut self) {}
}

// ═══════════════════════════════════════════════════════════════════════════
// PTY state structure
// ═══════════════════════════════════════════════════════════════════════════

/// Per-PTY bookkeeping structure.
///
/// One instance per PTY pair, shared between the master and slave halves.
pub struct Pty {
    /// State flags: TTY_ACTIVE, PTY_ACTIVE, TTY_CLOSED, PTY_CLOSED.
    state: u8,

    // ── Master read (master reading from output buffer) ────────────
    /// Endpoint of the process reading from the master.
    rdcaller: u32,
    /// ID of the suspended read request.
    rdid: u32,
    /// Grant for the reader's address space.
    rdgrant: u32,
    /// Bytes remaining to be read.
    rdleft: usize,
    /// Bytes transferred so far.
    rdcum: usize,

    // ── Master write (master writing data that goes to slave input) ─
    /// Endpoint of the process writing to the master.
    wrcaller: u32,
    /// ID of the suspended write request.
    wrid: u32,
    /// Grant for the writer's address space.
    wrgrant: u32,
    /// Bytes remaining to be written.
    wrleft: usize,
    /// Bytes transferred so far.
    wrcum: usize,

    // ── Output buffer (slave→master direction) ─────────────────────
    /// Circular output buffer.
    obuf: [u8; TTY_OUT_BYTES],
    /// Head index (next write position).
    ohead: usize,
    /// Tail index (next read position).
    otail: usize,
    /// Number of bytes in the output buffer.
    ocount: usize,

    // ── Select/notification ────────────────────────────────────────
    /// Select operations the master is interested in.
    select_ops: u32,
    /// Process to notify on select.
    select_proc: u32,
}

impl Pty {
    /// Create a new PTY in the initial (closed) state.
    pub const fn new() -> Self {
        Self {
            state: 0,
            rdcaller: u32::MAX,
            rdid: 0,
            rdgrant: 0,
            rdleft: 0,
            rdcum: 0,
            wrcaller: u32::MAX,
            wrid: 0,
            wrgrant: 0,
            wrleft: 0,
            wrcum: 0,
            obuf: [0u8; TTY_OUT_BYTES],
            ohead: 0,
            otail: 0,
            ocount: 0,
            select_ops: 0,
            select_proc: u32::MAX,
        }
    }

    /// Reset the PTY to its initial state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // ── State query helpers ────────────────────────────────────────

    /// Returns `true` if the TTY (slave) side is active.
    pub fn is_tty_active(&self) -> bool {
        self.state & TTY_ACTIVE != 0
    }

    /// Returns `true` if the PTY (master) side is active.
    pub fn is_pty_active(&self) -> bool {
        self.state & PTY_ACTIVE != 0
    }

    /// Returns `true` if the TTY side has been closed.
    pub fn is_tty_closed(&self) -> bool {
        self.state & TTY_CLOSED != 0
    }

    /// Returns `true` if the PTY side has been closed.
    pub fn is_pty_closed(&self) -> bool {
        self.state & PTY_CLOSED != 0
    }

    // ── Master-side operations ─────────────────────────────────────

    /// Open the master side.
    ///
    /// Returns `Err(Busy)` if already open, `Err(NotFound)` if the
    /// slave TTY is not initialized.
    pub fn master_open(&mut self) -> Result<(), DriverError> {
        if self.state & PTY_ACTIVE != 0 {
            return Err(DriverError::Busy);
        }
        self.state |= PTY_ACTIVE;
        self.rdcum = 0;
        self.wrcum = 0;
        Ok(())
    }

    /// Close the master side.
    ///
    /// If the slave is still active, signals SIGHUP and marks
    /// PTY_CLOSED.  If the slave is already closed, resets fully.
    /// Returns the host signal request, if any.
    pub fn master_close(&mut self, host: &mut dyn PtyHost) {
        if (self.state & (TTY_ACTIVE | TTY_CLOSED)) != TTY_ACTIVE {
            // Slave not active or already closed — full reset.
            self.reset();
        } else {
            self.state |= PTY_CLOSED;
            host.sigchar(1, true); // SIGHUP
        }
    }

    /// Initiate a read from the master side.
    ///
    /// Returns `Ok(Some(count))` if data was immediately available,
    /// `Ok(None)` if the read would block (suspend), or `Err(...)`.
    pub fn master_read(
        &mut self,
        size: usize,
        nonblock: bool,
        host: &mut dyn PtyHost,
    ) -> Result<Option<usize>, DriverError> {
        if self.state & TTY_CLOSED != 0 {
            return Ok(Some(0)); // EOF
        }

        if self.rdcaller != u32::MAX || self.rdleft != 0 || self.rdcum != 0 {
            return Err(DriverError::Busy);
        }

        if size == 0 {
            return Err(DriverError::InvalidArgument);
        }

        // Store the read request.
        self.rdleft = size;

        // Try to satisfy from the output buffer immediately.
        self.start_transfer();

        host.handle_events();

        if self.rdleft == 0 {
            self.rdcaller = u32::MAX;
            return Ok(Some(self.rdcum));
        }

        if nonblock {
            let r = if self.rdcum > 0 {
                Some(self.rdcum)
            } else {
                None // EAGAIN
            };
            self.rdleft = 0;
            self.rdcum = 0;
            self.rdcaller = u32::MAX;
            return Ok(r);
        }

        // Suspend — caller must provide rdcaller/rdgrant/rdid.
        Ok(None)
    }

    /// Complete a suspended master read by setting caller/grant/id.
    ///
    /// Must be called after `master_read` returns `Ok(None)`.
    pub fn set_read_call(&mut self, caller: u32, grant: u32, id: u32) {
        self.rdcaller = caller;
        self.rdgrant = grant;
        self.rdid = id;
    }

    /// Initiate a write to the master side.
    ///
    /// The written data will be processed by the TTY input layer and
    /// made available to the slave reader.
    ///
    /// Returns `Ok(true)` if data was immediately consumed,
    /// `Ok(false)` if the write would block (suspend).
    pub fn master_write(
        &mut self,
        size: usize,
        nonblock: bool,
        host: &mut dyn PtyHost,
    ) -> Result<bool, DriverError> {
        if self.state & TTY_CLOSED != 0 {
            return Err(DriverError::Io);
        }

        if self.wrcaller != u32::MAX || self.wrleft != 0 || self.wrcum != 0 {
            return Err(DriverError::Busy);
        }

        if size == 0 {
            return Err(DriverError::InvalidArgument);
        }

        self.wrleft = size;

        host.handle_events();

        if self.wrleft == 0 {
            self.wrcaller = u32::MAX;
            return Ok(true);
        }

        if nonblock {
            if self.wrcum > 0 {
                self.wrcum = 0;
            }
            self.wrleft = 0;
            self.wrcaller = u32::MAX;
            return Ok(false);
        }

        // Suspend — caller must set write call details.
        Ok(false)
    }

    /// Complete a suspended master write by setting caller/grant/id.
    pub fn set_write_call(&mut self, caller: u32, grant: u32, id: u32) {
        self.wrcaller = caller;
        self.wrgrant = grant;
        self.wrid = id;
    }

    /// Cancel a pending master-side I/O request.
    ///
    /// Returns `Some(bytes_transferred)` if a read was cancelled,
    /// `Some(bytes_transferred)` on write cancellation, or `None`
    /// if no matching request was found.
    pub fn master_cancel(&mut self, caller: u32, id: u32) -> Option<usize> {
        // Verify the caller/id match a pending request (C: pp->rdcaller == endpt && pp->rdid == id).
        if self.rdcaller == caller && self.rdid == id && (self.rdleft > 0 || self.rdcum > 0) {
            let r = if self.rdcum > 0 { self.rdcum } else { 0 };
            self.rdleft = 0;
            self.rdcum = 0;
            self.rdcaller = u32::MAX;
            return Some(r);
        }

        if self.wrcaller == caller && self.wrid == id && (self.wrleft > 0 || self.wrcum > 0) {
            let r = if self.wrcum > 0 { self.wrcum } else { 0 };
            self.wrleft = 0;
            self.wrcum = 0;
            self.wrcaller = u32::MAX;
            return Some(r);
        }

        // No matching request found (C: return EDONTREPLY).
        None
    }

    // ── Slave-side operations (called by the TTY server) ────────────

    /// The TTY (slave) side has been opened.
    pub fn slave_open(&mut self) {
        self.state |= TTY_ACTIVE;
        self.state &= !TTY_CLOSED;
    }

    /// The TTY (slave) side has been closed.
    ///
    /// If the master is still active, pending requests are cancelled
    /// and the state is marked TTY_CLOSED.
    pub fn slave_close(&mut self) {
        if (self.state & PTY_ACTIVE) == 0 {
            self.reset();
            return;
        }

        // Cancel any pending master read.
        self.rdleft = 0;
        self.rdcum = 0;
        self.rdcaller = u32::MAX;

        // Cancel any pending master write.
        self.wrleft = 0;
        self.wrcum = 0;
        self.wrcaller = u32::MAX;

        if self.state & PTY_CLOSED != 0 {
            self.reset();
        } else {
            self.state |= TTY_CLOSED;
        }
    }

    /// Slave read: transfer bytes from the master writer to TTY input.
    ///
    /// Called by the TTY server's `tty_devread` hook.  In the real
    /// implementation, each byte is copied from the writer's grant via
    /// `sys_safecopyfrom` and fed to `host.in_process()`.  Without the
    /// IPC grant infrastructure (Phase 12), we can only advance the
    /// bookkeeping here.  The `PtyHost` wiring will be done when grants
    /// are available (see `set_write_call` / `wrgrant`).
    pub fn slave_read(&mut self, try_only: bool, _host: &mut dyn PtyHost) -> bool {
        if self.state & PTY_CLOSED != 0 {
            if !try_only {
                // Signal EOF to the TTY reader.
            }
            return true;
        }

        if try_only {
            return self.wrleft > 0;
        }

        for _ in 0..64 {
            // Safety valve: process up to 64 bytes at a time
            if self.wrleft == 0 {
                break;
            }
            // In the real implementation, one byte would be copied
            // from the writer's grant and fed to in_process().
            // For now, mark one byte as consumed.
            self.wrcum += 1;
            self.wrleft -= 1;

            if self.wrleft == 0 {
                // Write completed, caller should reply.
                self.wrcum = 0;
                self.wrcaller = u32::MAX;
            }
        }

        false
    }

    /// Slave write: transfer bytes from TTY output to the PTY output buffer.
    ///
    /// Called by the TTY server's `tty_devwrite` hook.
    ///
    /// Returns `true` if space is available (`try` mode) or buffer has data.
    pub fn slave_write(&mut self, try_only: bool, src_data: &[u8], host: &mut dyn PtyHost) -> bool {
        if self.state & PTY_CLOSED != 0 {
            return true; // Error will be returned by caller
        }

        if try_only {
            return self.ocount < TTY_OUT_BYTES;
        }

        let mut src_offset = 0;
        while src_offset < src_data.len() {
            let space = TTY_OUT_BYTES - self.ocount;
            if space == 0 {
                break;
            }

            // How much contiguous space at ohead?
            let contiguous = if self.ohead < self.otail {
                self.otail - self.ohead
            } else {
                TTY_OUT_BYTES - self.ohead
            };
            let chunk_size = contiguous.min(space).min(src_data.len() - src_offset);

            // Copy into the buffer
            let dst = &mut self.obuf[self.ohead..self.ohead + chunk_size];
            dst.copy_from_slice(&src_data[src_offset..src_offset + chunk_size]);

            // Apply output processing via host
            let mut len = chunk_size;
            let mut ocount = chunk_size;
            host.out_process(
                &self.obuf[self.ohead..self.ohead + chunk_size],
                &mut len,
                &mut ocount,
            );

            // Update bookkeeping
            self.ocount += ocount;
            self.ohead = (self.ohead + ocount) % TTY_OUT_BYTES;
            src_offset += len;

            // Try to transfer to master reader
            self.start_transfer();
        }

        true
    }

    /// Echo one character to the output buffer.
    pub fn slave_echo(&mut self, c: u8, host: &mut dyn PtyHost) {
        let space = TTY_OUT_BYTES - self.ocount;
        if space == 0 {
            return;
        }

        self.obuf[self.ohead] = c;
        let mut len = 1;
        let mut ocount = 1;
        host.out_process(&self.obuf[self.ohead..=self.ohead], &mut len, &mut ocount);
        if len == 0 {
            return;
        }

        self.ocount += ocount;
        self.ohead = (self.ohead + ocount) % TTY_OUT_BYTES;
        self.start_transfer();
    }

    /// Cancel pending input (called by TTY icancel hook).
    pub fn slave_icancel(&mut self) {
        if self.wrleft > 0 {
            // Reply to the master writer with what we have.
            self.wrcum += self.wrleft;
            self.wrleft = 0;
            self.wrcaller = u32::MAX;
        }
    }

    /// Cancel pending output (called by TTY ocancel hook).
    pub fn slave_ocancel(&mut self) {
        self.ocount = 0;
        self.otail = self.ohead;
    }

    // ── Select ─────────────────────────────────────────────────────

    /// Check which select operations are ready on the master side.
    pub fn select_try(&self, ops: u32) -> u32 {
        let mut r = 0;

        if ops & 2 != 0 {
            // Write (CDEV_OP_WR) — ready if slave closed or there is any
            // pending write state (in progress or idle with active slave).
            if self.state & TTY_CLOSED != 0
                || self.wrleft != 0
                || self.wrcum != 0
                || self.state & TTY_ACTIVE != 0
            {
                r |= 2;
            }
        }

        if ops & 1 != 0 {
            // Read (CDEV_OP_RD) — ready if slave closed, pending read,
            // or data available in output buffer.
            if self.state & TTY_CLOSED != 0
                || self.rdleft != 0
                || self.rdcum != 0
                || self.ocount > 0
            {
                r |= 1;
            }
        }

        r
    }

    /// Register select interest on the master side.
    pub fn master_select(&mut self, ops: u32, watch: bool, endpt: u32) -> u32 {
        let ready = self.select_try(ops & 3); // only RD (1) + WR (2)

        let remaining = ops & !ready;
        if remaining != 0 && watch {
            self.select_ops |= remaining;
            self.select_proc = endpt;
        }

        ready
    }

    /// Re-evaluate select for the slave side and notify if ready.
    ///
    /// Computes ready ops and clears them from the interest set.
    /// The actual `chardriver_reply_select` notification is deferred
    /// until the character driver framework is available (see 12.24).
    pub fn select_retry(&mut self, _minor: u32) -> u32 {
        if self.select_ops == 0 {
            return 0;
        }
        let r = self.select_try(self.select_ops);
        if r != 0 {
            self.select_ops &= !r;
            // In real implementation, call chardriver_reply_select
        }
        r
    }

    // ── Internal helpers ───────────────────────────────────────────

    /// Transfer bytes from the output buffer to the master reader.
    fn start_transfer(&mut self) {
        loop {
            let count = self.ocount.min(self.rdleft);
            if count == 0 {
                break;
            }
            // In real implementation, copy from obuf[otail..] to
            // the reader's grant via sys_safecopyto.
            // For now, just advance the bookkeeping.
            self.ocount -= count;
            self.otail = (self.otail + count) % TTY_OUT_BYTES;
            self.rdcum += count;
            self.rdleft -= count;
        }
    }

    /// Number of bytes in the output buffer.
    pub fn output_count(&self) -> usize {
        self.ocount
    }

    /// Returns `true` if there is a pending master read.
    pub fn has_pending_read(&self) -> bool {
        self.rdleft > 0 || self.rdcum > 0
    }

    /// Returns `true` if there is a pending master write.
    pub fn has_pending_write(&self) -> bool {
        self.wrleft > 0 || self.wrcum > 0
    }
}

// ── Default impl ──────────────────────────────────────────────────────────

impl Default for Pty {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PTY table — one per pair
// ═══════════════════════════════════════════════════════════════════════════

/// Wrapper around `UnsafeCell<Pty>` that implements `Sync`.
/// Safe because PTY access is single-threaded (only the TTY server).
struct PtyCell(UnsafeCell<Pty>);

// Safety: PTY table is only accessed from the TTY server thread.
unsafe impl Sync for PtyCell {}

impl PtyCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(Pty::new()))
    }

    /// Get a raw pointer to the inner `Pty`.
    fn get(&self) -> *mut Pty {
        self.0.get()
    }
}

/// Global PTY table (one entry per PTY pair).
static PTY_TABLE: [PtyCell; NR_PTYS] = [
    PtyCell::new(),
    PtyCell::new(),
    PtyCell::new(),
    PtyCell::new(),
];

/// Initialize the PTY table (called once at boot).  Currently a no-op
/// because the array is const-initialized; individual PTYs are
/// initialized via [`pty_init`].
pub fn pty_table_init() {}

/// Get a raw pointer to the PTY at the given index.
///
/// # Safety
///
/// Caller must ensure no concurrent mutable access to the same index.
unsafe fn pty_ptr(index: usize) -> *mut Pty {
    assert!(index < NR_PTYS, "PTY index out of range");
    // Safety: UnsafeCell gives us interior mutability; caller ensures
    // exclusive access per index.
    PTY_TABLE[index].get()
}

/// Access a PTY by index (mutable reference, single-threaded usage).
///
/// # Safety
///
/// Caller must ensure exclusive access (no concurrent reads/writes).
pub unsafe fn pty_by_index(index: usize) -> &'static mut Pty {
    // Safety: caller guarantees exclusive access.
    unsafe { &mut *pty_ptr(index) }
}

/// Convert a minor number to a PTY index and side (master/slave).
pub fn minor_to_pty(minor: u32) -> Option<(usize, bool)> {
    if minor >= PTYPX_MINOR && minor < PTYPX_MINOR + NR_PTYS as u32 {
        Some(((minor - PTYPX_MINOR) as usize, true)) // master
    } else if minor >= TTYPX_MINOR && minor < TTYPX_MINOR + NR_PTYS as u32 {
        Some(((minor - TTYPX_MINOR) as usize, false)) // slave
    } else {
        None
    }
}

/// Initialize a PTY for the given TTY line index.
///
/// Sets up the PTY state and returns a reference to it.
///
/// # Safety
///
/// Must be called once per PTY during TTY server init, with
/// exclusive access to the PTY table.
pub unsafe fn pty_init(index: usize) -> &'static mut Pty {
    assert!(index < NR_PTYS);
    // Safety: exclusive access guaranteed by caller during init.
    let pp = unsafe { &mut *pty_ptr(index) };
    pp.reset();
    pp
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock host that does minimal processing.
    struct MockHost {
        in_count: usize,
        out_count: usize,
        sig_count: usize,
        events_count: usize,
        in_buf: [u8; 256],
        in_len: usize,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                in_count: 0,
                out_count: 0,
                sig_count: 0,
                events_count: 0,
                in_buf: [0u8; 256],
                in_len: 0,
            }
        }
    }

    impl PtyHost for MockHost {
        fn in_process(&mut self, data: &[u8]) -> usize {
            let n = data.len().min(self.in_buf.len() - self.in_len);
            self.in_buf[self.in_len..self.in_len + n].copy_from_slice(&data[..n]);
            self.in_len += n;
            self.in_count += n;
            n
        }

        fn out_process(&mut self, _buf: &[u8], len: &mut usize, _ocount: &mut usize) {
            self.out_count += *len;
        }

        fn sigchar(&mut self, _sig: u8, _may_flush: bool) {
            self.sig_count += 1;
        }

        fn handle_events(&mut self) {
            self.events_count += 1;
        }
    }

    #[test]
    fn test_pty_new_state() {
        let p = Pty::new();
        assert!(!p.is_tty_active());
        assert!(!p.is_pty_active());
        assert!(!p.is_tty_closed());
        assert!(!p.is_pty_closed());
        assert_eq!(p.output_count(), 0);
    }

    #[test]
    fn test_minor_to_pty() {
        // PTY master minors
        assert_eq!(minor_to_pty(PTYPX_MINOR), Some((0, true)));
        assert_eq!(minor_to_pty(PTYPX_MINOR + 1), Some((1, true)));
        assert_eq!(minor_to_pty(PTYPX_MINOR + 3), Some((3, true)));
        // PTY slave minors
        assert_eq!(minor_to_pty(TTYPX_MINOR), Some((0, false)));
        assert_eq!(minor_to_pty(TTYPX_MINOR + 2), Some((2, false)));
        // Out of range
        assert_eq!(minor_to_pty(0), None);
        assert_eq!(minor_to_pty(127), None);
        assert_eq!(minor_to_pty(255), None);
    }

    #[test]
    fn test_master_open_close() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        assert!(p.is_pty_active());

        // Double open must fail
        assert_eq!(p.master_open(), Err(DriverError::Busy));

        // Close without slave active should reset
        p.master_close(&mut host);
        assert!(!p.is_pty_active());
        assert_eq!(host.sig_count, 0); // no signal because slave wasn't active
    }

    #[test]
    fn test_master_close_with_active_slave() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        assert!(p.is_tty_active());

        // Close master while slave is active
        p.master_close(&mut host);
        assert!(p.is_pty_closed());
        assert!(p.is_tty_active());
        assert_eq!(host.sig_count, 1);
    }

    #[test]
    fn test_slave_open_close() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        // Open master
        assert!(p.master_open().is_ok());

        // Open slave
        p.slave_open();
        assert!(p.is_tty_active());

        // Close slave while master is active
        p.slave_close();
        assert!(p.is_tty_closed());
        assert!(p.is_pty_active());

        // Close master — should reset since both closed
        p.master_close(&mut host);
        assert!(!p.is_pty_active());
    }

    #[test]
    fn test_master_read_eof() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_close(); // TTY_CLOSED

        // Read should return EOF (0 bytes)
        let r = p.master_read(100, false, &mut host);
        assert_eq!(r, Ok(Some(0)));
    }

    #[test]
    fn test_output_buffer_operations() {
        let mut p = Pty::new();

        assert_eq!(p.output_count(), 0);

        // Write some data to the output buffer via slave_write
        let data = b"hello";
        let mut host = MockHost::new();
        p.slave_write(false, data, &mut host);

        assert!(p.output_count() > 0);
        assert_eq!(host.out_count, data.len());
    }

    #[test]
    fn test_master_read_from_buffer() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());

        // Write data from slave side
        let data = b"hello world";
        p.slave_write(false, data, &mut host);

        // Read from master side — request exact amount available
        let r = p.master_read(11, false, &mut host);
        assert_eq!(r, Ok(Some(11)));
    }

    #[test]
    fn test_master_read_nonblock_no_data() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());

        // No data available, non-blocking
        let r = p.master_read(100, true, &mut host);
        assert_eq!(r, Ok(None)); // EAGAIN
    }

    #[test]
    fn test_master_write_without_slave() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        // Slave is not open — write submits but suspends (no TTY input queue
        // to accept the data yet).
        let r = p.master_write(100, false, &mut host);
        assert_eq!(r, Ok(false)); // suspends
    }

    #[test]
    fn test_select_pty() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();

        // Initially nothing ready (no data)
        let ready = p.select_try(3); // RD | WR
        assert_eq!(ready, 2); // WR should be ready (can write to master)

        // Add data to output buffer
        p.slave_write(false, b"data", &mut host);

        // Now both RD and WR should be ready
        let ready = p.select_try(3);
        assert_eq!(ready, 3);
    }

    #[test]
    fn test_master_cancel_read() {
        let mut p = Pty::new();

        assert!(p.master_open().is_ok());
        p.slave_open();

        // Initiate a read (will suspend since no data)
        p.rdleft = 100;
        p.rdcaller = 42;
        p.rdid = 1;
        p.rdgrant = 100;

        // Cancel the read
        let r = p.master_cancel(42, 1);
        assert_eq!(r, Some(0)); // cancelled with 0 bytes
        assert_eq!(p.rdleft, 0);
        assert_eq!(p.rdcaller, u32::MAX);
    }

    #[test]
    fn test_master_cancel_write() {
        let mut p = Pty::new();

        assert!(p.master_open().is_ok());
        p.slave_open();

        // Initiate a write
        p.wrleft = 100;
        p.wrcaller = 42;
        p.wrid = 1;
        p.wrgrant = 200;

        // Cancel the write
        let r = p.master_cancel(42, 1);
        assert_eq!(r, Some(0));
        assert_eq!(p.wrleft, 0);
        assert_eq!(p.wrcaller, u32::MAX);
    }

    #[test]
    fn test_pty_index_access() {
        // Verify pty_init creates a valid reference
        unsafe {
            let pp = pty_init(0);
            assert!(!pp.is_pty_active());
            assert!(!pp.is_tty_active());
        }
    }

    #[test]
    fn test_pty_table_init() {
        // Should not panic
        pty_table_init();
    }

    #[test]
    fn test_slave_echo() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();

        p.slave_echo(b'A', &mut host);
        assert!(p.output_count() > 0);
    }

    #[test]
    fn test_slave_icancel() {
        let mut p = Pty::new();

        assert!(p.master_open().is_ok());
        p.slave_open();

        // Set up a pending write
        p.wrleft = 50;
        p.wrcaller = 10;

        p.slave_icancel();
        assert_eq!(p.wrleft, 0);
        assert_eq!(p.wrcaller, u32::MAX);
    }

    #[test]
    fn test_slave_ocancel() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());

        // Add some data
        p.slave_write(false, b"data", &mut host);
        assert!(p.output_count() > 0);

        // Cancel output
        p.slave_ocancel();
        assert_eq!(p.output_count(), 0);
    }

    #[test]
    fn test_master_open_twice_fails() {
        let mut p = Pty::new();
        assert!(p.master_open().is_ok());
        assert_eq!(p.master_open(), Err(DriverError::Busy));
    }

    #[test]
    fn test_select_after_slave_close() {
        let mut p = Pty::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_close();

        // After slave close, RD should be ready (EOF)
        let ready = p.select_try(1); // RD only
        assert_eq!(ready, 1);
    }

    #[test]
    fn test_consecutive_master_reads() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());

        // Write data in two chunks
        p.slave_write(false, b"hello ", &mut host);
        p.slave_write(false, b"world", &mut host);

        // Read exact amount available
        let r = p.master_read(11, false, &mut host);
        assert_eq!(r, Ok(Some(11)));
    }

    #[test]
    fn test_read_after_slave_close_returns_eof() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_close();

        let r = p.master_read(10, false, &mut host);
        assert_eq!(r, Ok(Some(0))); // EOF
    }

    #[test]
    fn test_write_fails_when_slave_closed() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_close();

        let r = p.master_write(10, false, &mut host);
        assert_eq!(r, Err(DriverError::Io));
    }

    #[test]
    fn test_reopen_after_full_close() {
        let mut p = Pty::new();
        let mut host = NoopHost;

        // Full open/close cycle
        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_close();
        p.master_close(&mut host);

        // Reopen
        assert!(p.master_open().is_ok());
        assert!(p.is_pty_active());
        assert!(!p.is_tty_active());
    }

    #[test]
    fn test_pty_const_new() {
        // Verify const fn works
        const _P: Pty = Pty::new();
    }

    #[test]
    fn test_select_retry_no_ops() {
        let mut p = Pty::new();
        assert_eq!(p.select_retry(PTYPX_MINOR), 0);
    }

    #[test]
    fn test_reset() {
        let mut p = Pty::new();
        let mut host = MockHost::new();

        assert!(p.master_open().is_ok());
        p.slave_open();
        p.slave_write(false, b"data", &mut host);
        assert!(p.output_count() > 0);

        p.reset();
        assert!(!p.is_pty_active());
        assert!(!p.is_tty_active());
        assert_eq!(p.output_count(), 0);
    }

    #[test]
    fn test_minor_ranges() {
        // Verify all valid minors map correctly
        for i in 0..NR_PTYS {
            let master_minor = PTYPX_MINOR + i as u32;
            let slave_minor = TTYPX_MINOR + i as u32;
            assert_eq!(minor_to_pty(master_minor), Some((i, true)));
            assert_eq!(minor_to_pty(slave_minor), Some((i, false)));
        }
    }
}
