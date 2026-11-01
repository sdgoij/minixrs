#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const PAGE_LEN: usize = 4096;

/// One page, so the two statics below cannot share a frame: a store to `PAGE`
/// must not be able to drag `WATCH`'s contents across the fork boundary.
#[repr(align(4096))]
struct Page([u8; PAGE_LEN]);

/// Written by both sides after the fork. Each side must keep seeing its own
/// write: neither may reach the other's view.
static mut PAGE: Page = Page([0xAA; PAGE_LEN]);

/// Only the parent writes this after the fork; the child only reads it. The
/// child must keep reading the fork-time contents, because fork is a snapshot
/// and the parent keeps running.
static mut WATCH: Page = Page([0xAA; PAGE_LEN]);

/// Only the *kernel* writes this page, in the child: a file read copied into it
/// through the child's page tables. It is still the fork-time shared frame when
/// that happens, so the kernel has to break the copy itself; otherwise the bytes
/// land in the frame the parent still maps.
static mut SINK: Page = Page([0xAA; PAGE_LEN]);

fn page_matches(page: *const u8, byte: u8) -> bool {
    unsafe { (0..PAGE_LEN).all(|i| core::ptr::read_volatile(page.add(i)) == byte) }
}

fn page_fill(page: *mut u8, byte: u8) {
    unsafe {
        for i in 0..PAGE_LEN {
            core::ptr::write_volatile(page.add(i), byte);
        }
    }
}

fn page_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(PAGE.0) as *mut u8 }
}

fn watch_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(WATCH.0) as *mut u8 }
}

fn sink_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(SINK.0) as *mut u8 }
}

/// Have the kernel copy a file into `SINK` on our behalf.
///
/// A page-sized read cannot be answered inside the reply message, so the
/// filesystem copies it in with a grant and the kernel performs the store
/// through this process's page tables. `/bin/forktest` is a `BOOT_BINS` entry,
/// so every image carries the file being read.
fn kernel_fills_sink() -> Result<(), i32> {
    let fd = unsafe { minix_std::fs::open(b"/bin/forktest", minix_std::fs::O_RDONLY, 0) }
        .map_err(|_| 1)?;
    let buf = unsafe { core::slice::from_raw_parts_mut(sink_ptr(), PAGE_LEN) };
    let n = unsafe { minix_std::fs::read(fd, buf) };
    let _ = minix_std::fs::close(fd);
    match n {
        Ok(n) if n > 0 => Ok(()),
        _ => Err(2),
    }
}

/// Long enough that a timer quantum lands inside it, so a child that ran before
/// its parent gets preempted and the parent's writes still land before the
/// check below.
fn spin() {
    unsafe {
        let mut counter: u64 = 0;
        let p = core::ptr::addr_of_mut!(counter);
        for _ in 0..2_000_000u64 {
            core::ptr::write_volatile(p, core::ptr::read_volatile(p) + 1);
        }
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    if !page_matches(page_ptr(), 0xAA)
        || !page_matches(watch_ptr(), 0xAA)
        || !page_matches(sink_ptr(), 0xAA)
    {
        userland::write_err(b"forktest: pre-fork page corrupt\n");
        return 1;
    }

    let pid = match unsafe { minix_std::process::fork() } {
        Ok(p) => p,
        Err(_) => {
            userland::write_err(b"forktest: fork failed\n");
            return 1;
        }
    };

    if pid == 0 {
        // A write the *kernel* makes for us has to break the copy too. Nothing in
        // this process has touched SINK, so it is still the frame the parent
        // maps when the read below is copied into it.
        if kernel_fills_sink().is_err() {
            userland::write_err(b"forktest: child could not read the image file\n");
            return 7;
        }
        let magic = unsafe {
            (
                core::ptr::read_volatile(sink_ptr()),
                core::ptr::read_volatile(sink_ptr().add(1)),
                core::ptr::read_volatile(sink_ptr().add(2)),
                core::ptr::read_volatile(sink_ptr().add(3)),
            )
        };
        if magic != (0x7F, b'E', b'L', b'F') {
            userland::write_err(b"forktest: the kernel's copy did not land in SINK\n");
            return 8;
        }
        userland::write_out(b"forktest: kernel wrote SINK, verified\n");

        // Child: the first store to PAGE must land in a private copy, not in
        // the frame the parent still maps.
        page_fill(page_ptr(), 0xBB);
        if !page_matches(page_ptr(), 0xBB) {
            userland::write_err(b"forktest: child write verify FAILED\n");
            return 2;
        }
        userland::write_out(b"forktest: child wrote 0xBB, verified\n");

        // Give the parent time to write WATCH, then check it is still what it
        // was at fork. Reading (never writing) WATCH is the point: a page the
        // child writes gets a private copy either way, so only a read of an
        // untouched page shows that the parent's write reached the child.
        spin();
        if !page_matches(watch_ptr(), 0xAA) {
            userland::write_err(
                b"forktest: parent's write reached the child (fork not a snapshot)\n",
            );
            return 6;
        }
        userland::write_out(b"forktest: child read its fork-time page, verified\n");
        0
    } else {
        // Parent: scribble WATCH while the child is running. A child that
        // aliases the frame reads this back instead of the fork-time contents.
        page_fill(watch_ptr(), 0xCC);

        // Then wait for the child and verify our own views: the child's write
        // never reached PAGE, and OURS is the one WATCH carries now.
        let (_, status) = match minix_std::process::waitpid(pid, 0) {
            Ok(w) => w,
            Err(_) => {
                userland::write_err(b"forktest: waitpid failed\n");
                return 1;
            }
        };
        if status != 0 {
            userland::write_err(b"forktest: child exited with status ");
            userland::print_dec(status as u32);
            userland::write_err(b"\n");
            return 3;
        }
        if !page_matches(page_ptr(), 0xAA) {
            userland::write_err(b"forktest: parent view corrupted by child (COW broken)\n");
            return 4;
        }
        if !page_matches(watch_ptr(), 0xCC) {
            userland::write_err(b"forktest: parent's own write did not land\n");
            return 5;
        }
        // The child had the kernel fill SINK and never wrote it itself, so the
        // parent's view has to be the fork-time page: a kernel store through the
        // shared frame would have put the file's bytes here too.
        if !page_matches(sink_ptr(), 0xAA) {
            userland::write_err(b"forktest: a kernel write reached the parent's frame\n");
            return 9;
        }
        userland::write_out(b"forktest: parent isolated, wrote 0xCC, verified\n");
        userland::write_out(b"forktest: all phases verified\n");
        0
    }
}
