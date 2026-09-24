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
    if !page_matches(page_ptr(), 0xAA) || !page_matches(watch_ptr(), 0xAA) {
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
        userland::write_out(b"forktest: parent isolated, wrote 0xCC, verified\n");
        userland::write_out(b"forktest: all phases verified\n");
        0
    }
}
