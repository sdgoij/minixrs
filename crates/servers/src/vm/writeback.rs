//! Writeback of `MAP_SHARED` file pages (KNOWN_ISSUES.md item 48).
//!
//! A store through a writable `MAP_SHARED` file mapping lands in the shared cache
//! frame, so every mapper sees it, but the file itself is untouched: nothing traps
//! the store, and no cache entry knows it is no longer the file's content. This
//! module is the other half. When such a region is released (`munmap`, process
//! exit) the cached pages of its file range are marked dirty, and a job writes
//! each one back through VFS (`VMVFSREQ_FDWRITE`).
//!
//! Two properties shape the implementation:
//!
//! * The write cannot be synchronous. VFS may be the reason VM is running at all
//!   (it sends the requests that complete VM's file pages), and a `SENDREC` from
//!   VM would absorb a request VFS made while VM waits, the deadlock the whole
//!   `vm/vfs_request.rs` protocol exists to avoid. So the request is asynchronous
//!   and a fixed pool of jobs carries the work across completions.
//! * The bytes cannot come from the mapping process. The writeback is issued
//!   while its region is being torn down, and at exit its page tables are destroyed
//!   before VFS can answer; a grant on that address space would resolve to nothing.
//!   VM maps the cache frame into its own address space instead and grants VFS a
//!   read from there, holding a `PhysBlock` reference on the frame until the answer
//!   arrives.
//!
//! Only the *cache* is scanned, and only at release. A page that is not cached
//! cannot be written back: a page past the file's end is never file content, and a
//! page the mapping never faulted has nothing of the mapping's in it.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use arch_common::com::{MEMFD_DEV, VM_PROC_NR, VMVFSREQ_FDWRITE};

use crate::vm::cache;
use crate::vm::pb;
use crate::vm::region;
use crate::vm::vfs_request::{self, Job};

const PAGE_SIZE: u64 = 4096;

/// How many released regions may be waiting for their writeback at once.
///
/// One per region whose pages are being written back, retired in order. A single
/// process cannot have more than [`region::MAX_REGIONS`] regions at exit, so this
/// covers a whole address space. Several processes exiting while a writeback is in
/// flight can still exhaust it: the write is then reported, and the fd handling falls
/// back to what the caller would have done before writeback existed.
const MAX_WRITEBACKS: usize = region::MAX_REGIONS;

/// One released region's file range, waiting to be written back.
struct WritebackJob {
    dev: u32,
    ino: u32,
    /// VM's file descriptor for the file (VFS's vmfd); the writes go through it,
    /// so it must not be closed until the job retires. `close` says who closes.
    fd: i32,
    /// File offsets of the range the released region covered.
    lo: u64,
    hi: u64,
    /// Next file offset to look for a dirty page at or after.
    cursor: u64,
    /// Send `FDCLOSE` for `fd` when the job retires. Set only by the release that
    /// saw no other region referencing the vmfd, which is by construction the last
    /// one queued for it (see `queue_region`).
    close: bool,
}

/// The page currently being written back.
struct Inflight {
    /// VM virtual address the cache frame is mapped at, granted to VFS.
    scratch: u64,
    /// The reference held on the frame while VFS reads it.
    pb: usize,
    phys: u64,
    dev: u32,
    ino: u32,
    ino_offset: u64,
}

struct Engine {
    /// Ring of pending jobs, oldest at `head`.
    jobs: [Option<WritebackJob>; MAX_WRITEBACKS],
    head: usize,
    count: usize,
    inflight: Option<Inflight>,
}

impl Engine {
    const fn new() -> Self {
        Self {
            jobs: [const { None }; MAX_WRITEBACKS],
            head: 0,
            count: 0,
            inflight: None,
        }
    }

    /// Queue `job` behind the ones already pending. False when the ring is full.
    fn enqueue(&mut self, job: WritebackJob) -> bool {
        if self.count == MAX_WRITEBACKS {
            return false;
        }
        let tail = (self.head + self.count) % MAX_WRITEBACKS;
        self.jobs[tail] = Some(job);
        self.count += 1;
        true
    }

    fn head_job(&mut self) -> Option<&mut WritebackJob> {
        if self.count == 0 {
            return None;
        }
        self.jobs[self.head].as_mut()
    }

    fn retire(&mut self) -> Option<WritebackJob> {
        if self.count == 0 {
            return None;
        }
        let job = self.jobs[self.head].take();
        self.head = (self.head + 1) % MAX_WRITEBACKS;
        self.count -= 1;
        job
    }
}

struct EngineCell(UnsafeCell<Engine>);
unsafe impl Sync for EngineCell {}

impl EngineCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(Engine::new()))
    }
}

static ENGINE: EngineCell = EngineCell::new();

fn engine() -> &'static mut Engine {
    unsafe { &mut *ENGINE.0.get() }
}

/// Is a shared, writable file region's content something the file should receive?
///
/// A memfd is excluded: its frames *are* the object, so there is no file to write
/// them to. A read-only mapping cannot be written through, so it owes nothing. A
/// region with no bytes inside the file (a mapping past EOF) likewise.
pub fn owes_writeback(flags: u32, dev: u32, fd: i32, file_offset: u64, file_size: u64) -> bool {
    flags & (region::VR_FILE | region::VR_SHARED | region::VR_WRITABLE)
        == (region::VR_FILE | region::VR_SHARED | region::VR_WRITABLE)
        && dev != MEMFD_DEV
        && fd >= 0
        && file_size > file_offset
}

/// Queue the writeback of a released region's file range.
///
/// `close` says the release found no other region referencing `fd`, so the job
/// owns closing it. Returns true when the caller must *not* close the fd itself:
/// a job took it over. False returns fall into two groups, both of which leave
/// the fd with the caller: there is nothing to write (no cached page in the range
/// is or was part of a shared mapping), or the caller's `close` is false because
/// another region still uses the vmfd.
///
/// A full job ring is the third case: the write is reported as lost and the close
/// is left to the caller (`close` is returned), so the vmfd cannot leak.
pub fn queue_region(
    flags: u32,
    dev: u32,
    ino: u32,
    fd: i32,
    file_offset: u64,
    file_size: u64,
    close: bool,
) -> bool {
    if !owes_writeback(flags, dev, fd, file_offset, file_size) {
        return false;
    }
    let lo = file_offset;
    let hi = file_size;
    // Dirty the pages this region could have written. A page another region already
    // dirtied stays dirty: its job has either not reached it yet or left it for a
    // later release.
    let marked = cache::cache_mark_range_dirty(dev, ino, lo, hi);
    if marked == 0 && cache::cache_find_dirty_after(dev, ino, lo, hi, lo).is_none() {
        return false;
    }
    let job = WritebackJob {
        dev,
        ino,
        fd,
        lo,
        hi,
        cursor: lo,
        close,
    };
    if !engine().enqueue(job) {
        // Nothing will close the fd, so answer the caller the same way a job
        // would: a release that found the vmfd unreferenced does the close.
        report(b"VM: writeback: no free job slot (MAX_WRITEBACKS reached)\n");
        return close;
    }
    pump();
    true
}

/// Start the next writeback page, if nothing is in flight.
///
/// Called when a region is released, when a writeback completes, and once per VM
/// main-loop turn: the last is what makes progress independent of anything else
/// arriving, which matters when the request could not be sent for lack of a
/// `vfs_request` slot.
pub fn pump() {
    loop {
        if engine().inflight.is_some() {
            return;
        }
        let Some(job) = engine().head_job() else {
            return;
        };
        let (dev, ino, lo, hi, cursor, fd) = (job.dev, job.ino, job.lo, job.hi, job.cursor, job.fd);

        let Some((off, phys, pb_idx)) = cache::cache_find_dirty_after(dev, ino, lo, hi, cursor)
        else {
            // The range is done. If this job owns the close, every write it made is
            // already in VFS's queue ahead of it, and no later job for the fd exists
            // (only the release that found the vmfd unreferenced asks for the close,
            // and by then no region is left to queue another).
            let Some(done) = engine().retire() else {
                return;
            };
            if done.close {
                vfs_request::close(done.fd);
            }
            continue;
        };

        // The frame has to be reachable from an address space that outlives the
        // process being torn down, so it is mapped into VM's own.
        let scratch = crate::vm::vm_mappage(
            phys,
            kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE,
        );
        if scratch == 0 || !pb::pb_ref(pb_idx) {
            if scratch != 0 {
                let _ = crate::vm::vm_unmappage(scratch);
            }
            report(b"VM: writeback: cannot map a cache frame, page skipped\n");
            advance(off);
            continue;
        }

        engine().inflight = Some(Inflight {
            scratch,
            pb: pb_idx,
            phys,
            dev,
            ino,
            ino_offset: off,
        });
        match vfs_request::send(
            VMVFSREQ_FDWRITE as i32,
            fd,
            VM_PROC_NR,
            off,
            scratch,
            PAGE_SIZE as u32,
            Job::Writeback,
        ) {
            Ok(()) => {
                advance(off);
                return;
            }
            Err(_) => {
                // No request slot. Undo the mapping and the reference; the cursor
                // stays on this page so the next pump retries it.
                engine().inflight = None;
                let _ = crate::vm::vm_unmappage(scratch);
                pb::pb_unref(pb_idx);
                return;
            }
        }
    }
}

/// Move the head job past the page at `off`.
fn advance(off: u64) {
    if let Some(job) = engine().head_job() {
        job.cursor = off + PAGE_SIZE;
    }
}

/// Handle VFS's answer to the write of the in-flight page.
///
/// `ok` is false when VFS reported a failure. The page is left dirty in that case:
/// its mapping is gone, so a later fault of the same page gets the same frame from
/// the cache, and the next release of a region covering it retries the write.
pub fn complete(ok: bool) {
    let Some(inf) = engine().inflight.take() else {
        report(b"VM: writeback: answer with no write in flight\n");
        return;
    };
    let _ = crate::vm::vm_unmappage(inf.scratch);
    pb::pb_unref(inf.pb);
    if ok {
        cache::cache_mark_written(inf.dev, inf.ino, inf.ino_offset, inf.phys);
    } else {
        report(b"VM: writeback: VFS could not write a shared page back\n");
    }
    pump();
}

/// Write a diagnostic line once, so a failing path cannot flood the console.
fn report(line: &[u8]) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    minix_rt::diag_write(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dev: u32, ino: u32, fd: i32, close: bool) -> WritebackJob {
        WritebackJob {
            dev,
            ino,
            fd,
            lo: 0,
            hi: 4096,
            cursor: 0,
            close,
        }
    }

    #[test]
    fn test_jobs_retire_in_the_order_they_were_queued() {
        // The fd close is carried by a job, so order is a correctness property: the
        // job that closes the vmfd must be the last one VFS is told about.
        let mut e = Engine::new();
        assert!(e.enqueue(job(1, 10, 3, false)));
        assert!(e.enqueue(job(1, 11, 4, true)));
        assert_eq!(e.head_job().unwrap().fd, 3);
        let first = e.retire().unwrap();
        assert_eq!((first.fd, first.close), (3, false));
        let second = e.retire().unwrap();
        assert_eq!((second.fd, second.close), (4, true));
        assert!(e.retire().is_none());
        assert!(e.head_job().is_none());
    }

    #[test]
    fn test_a_full_ring_refuses_instead_of_overwriting() {
        // A job that cannot be queued must be visible (`queue_region` reports it and
        // hands the close back to the caller), never a silent overwrite of a job that
        // still owes writes.
        let mut e = Engine::new();
        for i in 0..MAX_WRITEBACKS {
            assert!(e.enqueue(job(1, i as u32, i as i32, false)));
        }
        assert!(!e.enqueue(job(1, 999, 999, false)));
        assert_eq!(e.retire().unwrap().ino, 0);
        // The space the retire freed is reused, and at the back of the queue.
        assert!(e.enqueue(job(1, 999, 999, false)));
        let mut last = None;
        while let Some(j) = e.retire() {
            last = Some(j);
        }
        assert_eq!(last.unwrap().ino, 999);
    }

    #[test]
    fn test_only_a_shared_writable_file_region_owes_a_writeback() {
        let file = region::VR_FILE | region::VR_SHARED | region::VR_WRITABLE;
        assert!(owes_writeback(file, 7, 3, 0, 4096));
        // Read-only shared mapping: nothing can have been written through it.
        assert!(!owes_writeback(
            region::VR_FILE | region::VR_SHARED,
            7,
            3,
            0,
            4096
        ));
        // Private writable mapping: the write belongs to the process, not the file.
        assert!(!owes_writeback(
            region::VR_FILE | region::VR_WRITABLE,
            7,
            3,
            0,
            4096
        ));
        // A memfd's frames are the object; there is no file to write.
        assert!(!owes_writeback(file, MEMFD_DEV, 3, 0, 4096));
        // Nothing inside the file.
        assert!(!owes_writeback(file, 7, 3, 4096, 4096));
    }
}
