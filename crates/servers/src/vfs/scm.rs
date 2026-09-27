//! Descriptor passing (`SCM_RIGHTS`) for local-domain sockets.
//!
//! This is a deliberate deviation from MINIX 3.3.0. The reference's uds driver
//! performs the transfer itself, calling VFS `copyfd()` as a *back-call* while
//! VFS waits for that driver's ioctl reply (`ioc_uds.c` `send_fds`/`recv_fds`,
//! served by `filedes.c` `do_copyfd`). MINIX answers the back-call on another
//! VFS thread; this port's VFS is single-threaded and blocks in
//! `request::fs_sendrec`, so the call could never be answered. VFS therefore
//! owns the whole transfer — the layering Linux uses, where the transport
//! carries a rendezvous and the kernel owns the descriptors. See `WAYLAND.md`
//! §5.
//!
//! The rendezvous is the *receiving* socket's clone device, which is what the
//! reference effectively keys on too (it stores the descriptors in
//! `uds_fd_table[peer].ancillary_data`). A sender captures its descriptors — and
//! its credentials — into the peer's pending set; the receiver drains that set at
//! `recvmsg`, and it is released if the receiving socket is closed first, which
//! is the reference's `uds_clear_fds`.
//!
//! One deliberate second deviation, on `SCM_CREDS`: the reference appends a
//! credentials message to *every* `recvmsg` that has room, from an
//! `ancillary_data.cred` that is zeroed until a `sendmsg` fills it. A receiver
//! checking `cr_uid` before its peer has sent anything would therefore read
//! **uid 0**. This port appends `SCM_CREDS` only once a send has actually
//! recorded credentials, so an absent message means "not known", never "root".

use core::cell::UnsafeCell;

use arch_common::com::UDS_MAJOR;
use net::{
    CmsgError, MSG_CONTROL_LEN_OFF, MSG_CONTROL_MAX, MSG_CONTROL_SIZE, NWIOSUDSCTRL, UUCRED_SIZE,
    build_control, cmsg_space, parse_rights, uucred,
};

use crate::vfs::call::{SELF, sys_vircopy};
use crate::vfs::consts::*;
use crate::vfs::glo::{current_fp, vfs_global};
use crate::vfs::types::{Filp, Fproc};

/// Most descriptors one control message may carry. The reference caps its array
/// at `OPEN_MAX` and answers `EOVERFLOW` past it.
const MAX_PASSED_FDS: usize = OPEN_MAX;

/// Most receiving sockets with descriptors in flight at once; the uds driver
/// hands out at most eight clone minors.
const MAX_PENDING: usize = 8;

/// Descriptors captured for one receiving socket, awaiting `recvmsg`.
#[derive(Clone, Copy)]
struct PendingSet {
    in_use: bool,
    /// The receiving socket's full device number.
    dev: u32,
    /// Filp indices, each holding one reference taken from the sender.
    fds: [i32; MAX_PASSED_FDS],
    len: usize,
    /// The last sender's credentials, recorded at `sendmsg` the way the
    /// reference's `send_fds` calls `getnucred`. `None` until a sendmsg happens,
    /// which is where this port deliberately departs from the reference (below).
    cred: Option<(u32, u32)>,
}

impl PendingSet {
    const fn empty() -> Self {
        Self {
            in_use: false,
            dev: 0,
            fds: [-1; MAX_PASSED_FDS],
            len: 0,
            cred: None,
        }
    }
}

struct TableCell(UnsafeCell<[PendingSet; MAX_PENDING]>);
// Safety: VFS is single-threaded, so the table is reached only from its receive
// loop. The host tests drive the pure helpers with their own arrays.
unsafe impl Sync for TableCell {}

static PENDING: TableCell = TableCell(UnsafeCell::new([PendingSet::empty(); MAX_PENDING]));

fn table() -> &'static mut [PendingSet; MAX_PENDING] {
    unsafe { &mut *PENDING.0.get() }
}

// ---- the rendezvous table ----

/// The set for `dev`, creating one when the table has room.
fn set_for(sets: &mut [PendingSet], dev: u32) -> Option<&mut PendingSet> {
    let i = if let Some(i) = sets.iter().position(|s| s.in_use && s.dev == dev) {
        i
    } else {
        let i = sets.iter().position(|s| !s.in_use)?;
        sets[i] = PendingSet::empty();
        sets[i].in_use = true;
        sets[i].dev = dev;
        i
    };
    sets.get_mut(i)
}

/// The set for `dev`, if it has one.
fn set_of(sets: &mut [PendingSet], dev: u32) -> Option<&mut PendingSet> {
    sets.iter_mut().find(|s| s.in_use && s.dev == dev)
}

/// The `msg_controllen` field of a control struct.
fn read_len(ctrl: &[u8; MSG_CONTROL_SIZE]) -> usize {
    let field: [u8; 4] = ctrl[MSG_CONTROL_LEN_OFF..MSG_CONTROL_LEN_OFF + 4]
        .try_into()
        .unwrap_or([0; 4]);
    u32::from_ne_bytes(field) as usize
}

/// Set the `msg_controllen` field of a control struct.
fn write_len(ctrl: &mut [u8; MSG_CONTROL_SIZE], len: usize) {
    ctrl[MSG_CONTROL_LEN_OFF..MSG_CONTROL_LEN_OFF + 4].copy_from_slice(&(len as u32).to_ne_bytes());
}

// ---- the VFS-side transfer ----

/// Handle an intercepted control ioctl on a local-domain socket.
///
/// `dev` is the socket's device number, `user_ep`/`va` the calling process and
/// its `struct msg_control`.
pub fn control_ioctl(request: u32, dev: u32, user_ep: i32, va: u64) -> i32 {
    if request == NWIOSUDSCTRL {
        send_control(dev, user_ep, va)
    } else {
        recv_control(dev, user_ep, va)
    }
}

/// `sendmsg`: capture the caller's descriptors and credentials for its peer.
fn send_control(dev: u32, user_ep: i32, va: u64) -> i32 {
    let mut ctrl = [0u8; MSG_CONTROL_SIZE];
    let r = copy_in(user_ep, va, &mut ctrl);
    if r != 0 {
        return r;
    }
    let controllen = read_len(&ctrl);

    // The reference's `do_sendmsg` rejects a peerless socket before it looks at
    // the control data; asking the driver is also how this port learns which
    // device the descriptors are destined for, and the sender's credentials.
    let [peer_minor, uid, gid] =
        match crate::vfs::device::cdev_ioctl_payload(dev, NWIOSUDSCTRL, user_ep) {
            Ok(words) => words,
            Err(e) => return e,
        };
    let peer = (dev & 0xFFFF_0000) | (peer_minor as u32 & 0xFFFF);

    let mut fds = [-1i32; MAX_PASSED_FDS];
    let n = match parse_rights(&ctrl, controllen, &mut fds) {
        Ok(n) => n,
        // A control message that does not parse is EINVAL; one carrying more
        // descriptors than a message may hold is EOVERFLOW, as in the reference.
        Err(CmsgError::BadHeader) => return EINVAL,
        Err(CmsgError::TooMany) => return EOVERFLOW,
    };

    // Resolve every descriptor before taking any reference, so one bad fd
    // cannot leave a partly-built set behind.
    let mut idxs = [-1i32; MAX_PASSED_FDS];
    if n > 0 {
        let fp = unsafe { current_fp() };
        if fp.is_null() {
            return EFAULT;
        }
        for k in 0..n {
            let idx = unsafe { crate::vfs::filedes::get_filp(fds[k], &*fp) };
            if idx < 0 {
                return EBADF;
            }
            idxs[k] = idx;
        }
    }

    let Some(set) = set_for(table(), peer) else {
        return EMFILE;
    };
    if set.len + n > MAX_PASSED_FDS {
        return EOVERFLOW;
    }
    // The credentials are recorded even for a message carrying no descriptors,
    // which is what makes a plain `sendmsg` still tell the receiver who sent it.
    set.cred = Some((uid as u32, gid as u32));
    for &idx in &idxs[..n] {
        // The reference's `copyfd(COPYFD_FROM)`: hold a reference so the
        // descriptor outlives the sender's own close.
        filp_hold(idx);
        set.fds[set.len] = idx;
        set.len += 1;
    }
    OK
}

/// `recvmsg`: install the descriptors waiting on this socket into the caller.
fn recv_control(dev: u32, user_ep: i32, va: u64) -> i32 {
    let mut ctrl = [0u8; MSG_CONTROL_SIZE];
    let r = copy_in(user_ep, va, &mut ctrl);
    if r != 0 {
        return r;
    }
    // The caller's `msg_controllen` is the room it offers. The reference clamps
    // it to `MSG_CONTROL_MAX` and answers `EOVERFLOW` when what is waiting does
    // not fit, leaving it pending for a later call.
    let avail = read_len(&ctrl).min(MSG_CONTROL_MAX);

    let Some(set) = set_of(table(), dev) else {
        write_len(&mut ctrl, 0);
        return copy_out(user_ep, va, &ctrl);
    };
    let n = set.len;
    if cmsg_space(4 * n) > avail {
        return EOVERFLOW;
    }
    let cred = set.cred;

    let fp = unsafe { current_fp() };
    if fp.is_null() {
        return EFAULT;
    }
    // Moving the captured references into the receiver needs a free slot each,
    // and a table that cannot take them all must not take some.
    if free_slots(fp) < n {
        return EMFILE;
    }
    // The credentials go in only when the caller left room for them, which is
    // the reference's `clen_desired <= clen_avail` test.
    let cred_fits = cmsg_space(4 * n) + cmsg_space(UUCRED_SIZE) <= avail;

    let mut out = [0i32; MAX_PASSED_FDS];
    for (k, &idx) in set.fds[..n].iter().enumerate() {
        out[k] = unsafe { install_fd(fp, idx) };
    }
    set.len = 0;
    // The descriptors are gone either way, but credentials the caller left no
    // room for stay pending and are offered again on the next call — the
    // reference keeps them in `ancillary_data.cred` for exactly that retry.
    set.in_use = cred.is_some() && !cred_fits;

    let cred_bytes = cred
        .filter(|_| cred_fits)
        .map(|(uid, gid)| uucred(uid, gid));
    let controllen = build_control(&mut ctrl, &out[..n], cred_bytes.as_ref());
    write_len(&mut ctrl, controllen);
    copy_out(user_ep, va, &ctrl)
}

/// Drop descriptors still in flight for a socket that is closing.
///
/// Called for every character-device close; only local sockets own a set. The
/// reference does this in `uds_clear_fds`.
pub fn release_device(dev: u32) {
    if (dev >> 16) != UDS_MAJOR {
        return;
    }
    let sets = table();
    let Some(i) = sets.iter().position(|s| s.in_use && s.dev == dev) else {
        return;
    };
    // Free the slot before dropping the references: a captured descriptor can
    // be another socket, whose close would re-enter here.
    let fds = sets[i].fds;
    let len = sets[i].len;
    sets[i] = PendingSet::empty();
    for &idx in &fds[..len] {
        if idx >= 0 {
            unsafe { crate::vfs::filedes::close_filp(idx) };
        }
    }
}

/// Take a reference on a filp, so a passed descriptor can outlive its sender.
fn filp_hold(filp_idx: i32) {
    if (filp_idx as usize) >= NR_FILPS {
        return;
    }
    unsafe {
        let glob = vfs_global();
        let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
        (*filp_arr.add(filp_idx as usize)).filp_count += 1;
    }
}

/// How many descriptors the process can still open.
fn free_slots(fp: *mut Fproc) -> usize {
    (0..OPEN_MAX)
        .filter(|&i| unsafe { (*fp).fp_filp[i] } < 0)
        .count()
}

/// Put a captured filp into the process's first free slot, moving the reference
/// the capture took rather than taking another.
///
/// # Safety
///
/// `free_slots(fp)` must have been at least one; the caller checks it.
unsafe fn install_fd(fp: *mut Fproc, filp_idx: i32) -> i32 {
    for i in 0..OPEN_MAX {
        if (*fp).fp_filp[i] < 0 {
            (*fp).fp_filp[i] = filp_idx;
            return i as i32;
        }
    }
    EMFILE
}

/// Copy the caller's control struct in.
fn copy_in(user_ep: i32, va: u64, dst: &mut [u8; MSG_CONTROL_SIZE]) -> i32 {
    unsafe { sys_vircopy(user_ep, va, SELF, dst.as_mut_ptr() as u64, MSG_CONTROL_SIZE) }
}

/// Copy the control struct back out.
fn copy_out(user_ep: i32, va: u64, src: &[u8; MSG_CONTROL_SIZE]) -> i32 {
    unsafe { sys_vircopy(SELF, src.as_ptr() as u64, user_ep, va, MSG_CONTROL_SIZE) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_accumulates_and_is_found_by_device() {
        let mut sets = [PendingSet::empty(); MAX_PENDING];
        {
            let s = set_for(&mut sets, 42).unwrap();
            s.fds[0] = 1;
            s.len = 1;
            s.cred = Some((1000, 100));
        }
        {
            let s = set_for(&mut sets, 42).unwrap();
            s.fds[1] = 2;
            s.len = 2;
            s.cred = Some((7, 8));
        }
        let s = set_of(&mut sets, 42).unwrap();
        assert_eq!(s.len, 2, "descriptors accumulate");
        assert_eq!(s.cred, Some((7, 8)), "the latest sender's credentials win");
        assert!(set_of(&mut sets, 43).is_none());
    }

    #[test]
    fn the_table_hands_out_one_slot_per_device_until_full() {
        let mut sets = [PendingSet::empty(); MAX_PENDING];
        for dev in 0..MAX_PENDING as u32 {
            assert!(set_for(&mut sets, dev).is_some());
        }
        assert!(set_for(&mut sets, 99).is_none());
        // Freeing one device makes room for the next.
        sets[0] = PendingSet::empty();
        assert!(set_for(&mut sets, 99).is_some());
    }
}
