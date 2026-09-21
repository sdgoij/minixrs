//! VM's requests to VFS, and the replies that complete them.
//!
//! VM asks VFS three things: resolve an fd (`FDLOOKUP`), read a file block into a page (`FDIO`),
//! and close an fd (`FDCLOSE`). The first two used to be a blocking `SENDREC`, which made VM wait
//! on VFS — and a `SENDREC`'s receive matches on source endpoint alone, so a request VFS made
//! *while* VM was waiting (a server growing its heap asks VM to mmap, finding 58) was delivered
//! into VM's reply slot. VFS then waited for a reply to a message VM had read as its own answer,
//! and neither side could move.
//!
//! C never blocks here: `vfs.c` sends with `asynsend3(..., AMF_NOREPLY)` and completes the request
//! in a callback when the answer arrives. `AMF_NOREPLY` is the other half of it — the kernel will
//! not let such a message satisfy the receive phase of a `SENDREC` (`try_one`), so VM's request
//! cannot be absorbed by a server that is itself waiting for VM.
//!
//! This is that shape, with two differences from C's `vfs.c`:
//!
//! * A fixed pool instead of `SLABALLOC`, because a request is enqueued from inside the page-fault
//!   path, and allocating there is the thing finding 58 is about.
//! * Several requests in flight, keyed by request id, instead of C's one-active LIFO queue. VFS
//!   echoes the id back (`vfs/call.rs` `vm_call_reply`, `VMV_REQID_OFF` 24 on both sides), so an
//!   answer is matched to the request that asked for it rather than to whichever request happened
//!   to be active — and a stray answer is *visible* instead of being read as the current one's.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use arch_common::com::{SUSPEND, VFS_PROC_NR, VM_VFS_REPLY};
use arch_common::ipc::{AMF_NOREPLY, Message};

use crate::vm::PageState;

// VM→VFS request protocol (VFS_VMCALL message; absolute message-byte offsets,
// matching `crates/servers/src/vfs/consts.rs`).
const VMCALL: i32 = 0x100 + 38; // VFS_BASE + 38
const VMCALL_REQ_OFF: usize = 16;
const VMCALL_FD_OFF: usize = 20;
const VMCALL_REQID_OFF: usize = 24;
const VMCALL_ENDPOINT_OFF: usize = 28;
const VMCALL_OFFSET_OFF: usize = 8;
const VMCALL_FAULTVA_OFF: usize = 32;
const VMCALL_LENGTH_OFF: usize = 48;

// Reply (VM_VFS_REPLY) payload offsets.
pub const VMV_ISDEV_OFF: usize = 12;
pub const VMV_RESULT_OFF: usize = 20;
pub const VMV_DEV_OFF: usize = 28;
pub const VMV_INO_OFF: usize = 32;
pub const VMV_FD_OFF: usize = 40;
pub const VMV_SIZE_PAGES_OFF: usize = 48;
// Device-mmap reply fields (FDLOOKUP of a char device): IS_DEVICE (u32, 1 = char
// device) at a byte range the file reply leaves free, phys/len overlapping
// dev/ino and the tail (meaningless for the file branch).
pub const VMV_PHYS_OFF: usize = 28;
pub const VMV_LEN_OFF: usize = 56;

/// How many requests may be outstanding at once.
///
/// One per parked page fault or pending `mmap`, so this is the number of processes that can be
/// waiting on VFS at the same time. Exhaustion is reported and the request fails rather than
/// blocking, which is the whole point of the module.
const MAX_INFLIGHT: usize = 16;

/// Operation not supported (ENOMEM from MINIX errno.h).
const ENOMEM: i32 = -12;

/// Input/output error (EIO).
const EIO: i32 = -5;

/// What to do when the answer arrives.
///
/// The page variant is deliberately the larger one: its state carries the pages still to map for
/// the fault it belongs to, and boxing it would allocate inside the fault path — the thing finding
/// 58 is about, and the reason this module has a fixed pool in the first place.
#[allow(clippy::large_enum_variant)]
pub enum Job {
    /// An `mmap` waiting on FDLOOKUP (C's `mmap_file_cont`). The state is the caller's original
    /// message: the fd, the length, the address hint and the flags are all in it, so the
    /// completion re-derives what it needs and then answers the caller itself.
    Mmap { ep: i32, caller_msg: [u8; 64] },
    /// A file page waiting on FDIO. The fault it belongs to travels in the state, because the page
    /// that lands last is what resolves that fault.
    Page(PageState),
}

/// One outstanding request: the id VFS will echo, and what to complete.
struct Node {
    busy: bool,
    reqid: u32,
    job: Option<Job>,
}

struct PoolCell(UnsafeCell<[Node; MAX_INFLIGHT]>);
unsafe impl Sync for PoolCell {}

impl PoolCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(
            [const {
                Node {
                    busy: false,
                    reqid: 0,
                    job: None,
                }
            }; MAX_INFLIGHT],
        ))
    }

    fn get(&self) -> *mut [Node; MAX_INFLIGHT] {
        self.0.get()
    }
}

static POOL: PoolCell = PoolCell::new();

/// Request ids start at 1: a fire-and-forget request (the async `FDCLOSE`) carries the 0 it was
/// sent with, and 0 must never match a node.
static NEXT_REQID: AtomicU32 = AtomicU32::new(1);

/// Send a request to VFS and return immediately.
///
/// `job` is stored with the request, so the answer completes what asked for it. The caller is
/// expected to return `SUSPEND` from its handler: the work it wanted done has not happened yet,
/// and its own reply goes out from the completion (or not at all, for a fault).
pub fn send(
    req: i32,
    fd: i32,
    ep: i32,
    offset: u64,
    fault_va: u64,
    length: u32,
    job: Job,
) -> Result<(), i32> {
    unsafe {
        let pool = &mut *POOL.get();
        let Some(slot) = pool.iter_mut().find(|n| !n.busy) else {
            report_pool_exhausted();
            return Err(ENOMEM);
        };

        let reqid = NEXT_REQID.fetch_add(1, Ordering::Relaxed);
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&VMCALL.to_le_bytes());
        msg[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4].copy_from_slice(&req.to_le_bytes());
        msg[VMCALL_FD_OFF..VMCALL_FD_OFF + 4].copy_from_slice(&fd.to_le_bytes());
        msg[VMCALL_REQID_OFF..VMCALL_REQID_OFF + 4].copy_from_slice(&reqid.to_le_bytes());
        msg[VMCALL_ENDPOINT_OFF..VMCALL_ENDPOINT_OFF + 4].copy_from_slice(&ep.to_le_bytes());
        msg[VMCALL_OFFSET_OFF..VMCALL_OFFSET_OFF + 8].copy_from_slice(&offset.to_le_bytes());
        msg[VMCALL_FAULTVA_OFF..VMCALL_FAULTVA_OFF + 8].copy_from_slice(&fault_va.to_le_bytes());
        msg[VMCALL_LENGTH_OFF..VMCALL_LENGTH_OFF + 4].copy_from_slice(&length.to_le_bytes());

        // Claim the slot before sending. `asynsend3` does not enter IPC — it hands the kernel a
        // table to read — so no answer can arrive before this call returns, and the alternative
        // order would leave the table describing a request that is already in flight.
        slot.busy = true;
        slot.reqid = reqid;
        slot.job = Some(job);

        if asyn_send(VFS_PROC_NR, &msg) != 0 {
            slot.busy = false;
            slot.job = None;
            return Err(EIO);
        }
        Ok(())
    }
}

/// Handle VFS's answer: complete the request it belongs to.
///
/// Returns `SUSPEND` — a reply is never answered, which is also what the sender's table expects.
pub fn complete(msg: &mut Message) -> i32 {
    let reply: [u8; 64] = unsafe { core::ptr::read((msg as *const Message).cast::<[u8; 64]>()) };

    if !is_vfs_reply(&reply) {
        report_mismatch(&reply);
        return SUSPEND;
    }

    let reqid = reply_reqid(&reply);
    // A request that wants no answer was sent with id 0 and dropped its answer on purpose (the
    // async `FDCLOSE`): there is nothing outstanding for it, and that is not a finding.
    if reqid == 0 {
        return SUSPEND;
    }

    match take_job(reqid) {
        Some(Job::Mmap { ep, mut caller_msg }) => {
            let status = crate::vm::finish_mmap_file(ep, &mut caller_msg, &reply);
            reply_to(ep, &mut caller_msg, status);
        }
        Some(Job::Page(state)) => crate::vm::finish_file_page(state, &reply),
        None => report_unmatched(reqid),
    }
    SUSPEND
}

/// Read the request id out of a reply.
fn reply_reqid(reply: &[u8; 64]) -> u32 {
    u32::from_le_bytes(
        reply[VMCALL_REQID_OFF..VMCALL_REQID_OFF + 4]
            .try_into()
            .unwrap_or([0; 4]),
    )
}

/// Take the job for `reqid`, freeing the slot. `None` when nothing is outstanding for that id.
fn take_job(reqid: u32) -> Option<Job> {
    unsafe {
        let pool = &mut *POOL.get();
        for slot in pool.iter_mut() {
            if slot.busy && slot.reqid == reqid {
                slot.busy = false;
                return slot.job.take();
            }
        }
    }
    None
}

/// True if `msg` is the reply VM's requests are waiting for.
///
/// A `SENDREC` consumes whatever its destination sends, which is how a request of VFS's own came
/// to be read as a result; the type is the first thing that says whether a message is an answer at
/// all, and the request id (below) is the one that says *whose* answer.
fn is_vfs_reply(msg: &[u8; 64]) -> bool {
    i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4])) == VM_VFS_REPLY as i32
}

/// Send the answer to a request whose handler returned without one — C's `reply()` from inside a
/// callback. Not reached on a host build, where nothing issues requests.
fn reply_to(ep: i32, msg: &mut [u8; 64], status: i32) {
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (ep, msg, status);
    }
    #[cfg(target_os = "minix")]
    unsafe {
        msg[4..8].copy_from_slice(&status.to_le_bytes());
        minix_rt::syscall2(minix_rt::SENDNB_CALL, ep as u64, msg.as_mut_ptr() as u64);
    }
}

/// Ask VFS to close an fd, without waiting for an answer.
///
/// Matching C `fdref.c` `fdref_deref` ("asynchronously close the fd in VFS ... a close failing,
/// although unexpected, isn't a problem") — and required for correctness: a close that waits
/// deadlocks during exec, where VFS is blocked in its own request to VM and would read the close
/// as the answer to that. VFS processes it when it returns to its main loop, and its answer
/// carries the id 0 this was sent with, so `complete` drops it.
pub fn close(fd: i32) {
    let mut msg = [0u8; 64];
    msg[4..8].copy_from_slice(&VMCALL.to_le_bytes());
    msg[VMCALL_REQ_OFF..VMCALL_REQ_OFF + 4]
        .copy_from_slice(&(arch_common::com::VMVFSREQ_FDCLOSE as i32).to_le_bytes());
    msg[VMCALL_FD_OFF..VMCALL_FD_OFF + 4].copy_from_slice(&fd.to_le_bytes());
    msg[VMCALL_ENDPOINT_OFF..VMCALL_ENDPOINT_OFF + 4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
    let _ = asyn_send(VFS_PROC_NR, &msg);
}

/// Hand VFS's kernel a table holding `msg`, so the message is delivered when VFS next receives.
///
/// `AMF_NOREPLY` is not decoration: it is what stops the kernel delivering this into the receive
/// phase of a `SENDREC` VFS is in (finding 58's other direction).
fn asyn_send(dst: i32, msg: &[u8; 64]) -> i32 {
    #[cfg(target_os = "minix")]
    {
        unsafe { minix_rt::asynsend3(dst, msg.as_ptr(), AMF_NOREPLY) }
    }
    #[cfg(not(target_os = "minix"))]
    {
        // A host build has no VFS to send to. `send` reports the failure to its caller, which is
        // what a server in that position has to do anyway.
        let _ = (dst, msg, AMF_NOREPLY);
        -1
    }
}

/// A byte line built without `alloc`, for diagnostics a server writes itself.
struct Line {
    buf: [u8; 160],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buf: [0; 160],
            len: 0,
        }
    }

    /// Append as much of `bytes` as fits. Reports are the thing being written when they are
    /// written, so a full buffer truncates rather than panics.
    fn push(&mut self, bytes: &[u8]) {
        let room = self.buf.len() - self.len;
        let n = room.min(bytes.len());
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn push_i32(&mut self, v: i32) {
        if v < 0 {
            self.push(b"-");
        }
        // Via i64 so i32::MIN's magnitude does not overflow.
        let mut n = i64::from(v).unsigned_abs();
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.push(&digits[i..]);
    }

    fn push_hex32(&mut self, mut v: u32) {
        self.push(b"0x");
        let mut digits = [0u8; 8];
        let mut i = digits.len();
        loop {
            i -= 1;
            let d = (v & 0xf) as u8;
            digits[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            v >>= 4;
            if v == 0 {
                break;
            }
        }
        self.push(&digits[i..]);
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// The report for a message whose type is not VM_VFS_REPLY: who it came from, what it was, and the
/// first payload words, so the log names the conversation that went wrong rather than only that
/// one did.
fn mismatch_line(msg: &[u8; 64], reqid: u32) -> Line {
    let src = i32::from_le_bytes(msg[0..4].try_into().unwrap_or([0; 4]));
    let mtype = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    let w0 = u32::from_le_bytes(msg[8..12].try_into().unwrap_or([0; 4]));
    let w1 = u32::from_le_bytes(msg[12..16].try_into().unwrap_or([0; 4]));

    let mut line = Line::new();
    line.push(b"VM: vfs_reply: type ");
    line.push_hex32(mtype as u32);
    line.push(b" from ep ");
    line.push_i32(src);
    line.push(b" is not VM_VFS_REPLY ");
    line.push_hex32(VM_VFS_REPLY);
    line.push(b" (reqid ");
    line.push_i32(reqid as i32);
    line.push(b", words ");
    line.push_hex32(w0);
    line.push(b" ");
    line.push_hex32(w1);
    line.push(b")\n");
    line
}

/// The report for an answer nobody asked for: a duplicate, or one for a request this VM has
/// already completed and forgotten.
fn unmatched_line(reqid: u32) -> Line {
    let mut line = Line::new();
    line.push(b"VM: vfs_reply: no request outstanding for reqid ");
    line.push_i32(reqid as i32);
    line.push(b"\n");
    line
}

/// Report a reply of the wrong type, once.
fn report_mismatch(msg: &[u8; 64]) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    minix_rt::diag_write(mismatch_line(msg, reply_reqid(msg)).as_bytes());
}

/// Report a reply with no request behind it, once.
fn report_unmatched(reqid: u32) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    minix_rt::diag_write(unmatched_line(reqid).as_bytes());
}

/// Report every request slot being in use, once.
fn report_pool_exhausted() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    minix_rt::diag_write(b"VM: vfs_request: no free request slot (MAX_INFLIGHT reached)\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool is a static, so these tests share it; each leaves it as it found it.
    #[test]
    fn test_reply_must_say_it_is_a_reply() {
        let mut reply = [0u8; 64];
        reply[4..8].copy_from_slice(&(VM_VFS_REPLY as i32).to_le_bytes());
        assert!(is_vfs_reply(&reply));

        // A request for the same conversation, delivered into the reply slot, is the case this
        // check exists for.
        let mut request = [0u8; 64];
        request[4..8].copy_from_slice(&VMCALL.to_le_bytes());
        assert!(!is_vfs_reply(&request));
        assert!(!is_vfs_reply(&[0u8; 64]));
    }

    #[test]
    fn test_mismatch_line_names_the_conversation() {
        // The line spells the two message types in hex, so the values are pinned here too: a
        // change to either constant then fails this test instead of turning the log line into a
        // quiet lie.
        assert_eq!(VM_VFS_REPLY, 0xc1e);
        assert_eq!(VMCALL, 0x126);

        let mut msg = [0u8; 64];
        msg[0..4].copy_from_slice(&VFS_PROC_NR.to_le_bytes());
        msg[4..8].copy_from_slice(&VMCALL.to_le_bytes());
        msg[8..12].copy_from_slice(&7u32.to_le_bytes());
        msg[12..16].copy_from_slice(&0xdead_beefu32.to_le_bytes());

        let line = mismatch_line(&msg, 3);
        assert_eq!(
            core::str::from_utf8(line.as_bytes()),
            Ok(
                "VM: vfs_reply: type 0x126 from ep 1 is not VM_VFS_REPLY 0xc1e \
                (reqid 3, words 0x7 0xdeadbeef)\n"
            )
        );
    }

    #[test]
    fn test_line_truncates_rather_than_panicking() {
        let mut line = Line::new();
        for _ in 0..64 {
            line.push(b"0123456789");
        }
        assert_eq!(line.as_bytes().len(), 160);
    }

    #[test]
    fn test_take_job_matches_the_request_id_and_frees_the_slot() {
        unsafe {
            let pool = &mut *POOL.get();
            for (i, slot) in pool.iter_mut().enumerate() {
                slot.busy = true;
                slot.reqid = 100 + i as u32;
                slot.job = Some(Job::Mmap {
                    ep: i as i32,
                    caller_msg: [0u8; 64],
                });
            }
        }

        // An id nobody asked about matches nothing...
        assert!(take_job(999).is_none());
        // ...and taking the same id twice takes one job, not two: a duplicate answer cannot
        // complete a request that is already done.
        assert!(take_job(103).is_some());
        assert!(take_job(103).is_none());
        // The id is what matched, not the position.
        match take_job(101) {
            Some(Job::Mmap { ep, .. }) => assert_eq!(ep, 1),
            Some(Job::Page(_)) => panic!("reqid 101 was an mmap"),
            None => panic!("reqid 101 was outstanding"),
        }

        unsafe {
            let pool = &mut *POOL.get();
            for slot in pool.iter_mut() {
                slot.busy = false;
                slot.reqid = 0;
                slot.job = None;
            }
        }
    }

    #[test]
    fn test_reply_reqid_is_the_message_word_vfs_echoes() {
        // `vfs/consts.rs` writes the id it read from the request into the reply, at the same
        // offset — 24 on both sides.
        let mut reply = [0u8; 64];
        reply[VMCALL_REQID_OFF..VMCALL_REQID_OFF + 4]
            .copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(reply_reqid(&reply), 0x1234_5678);
    }
}
