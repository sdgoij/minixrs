#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

const PAGE_LEN: usize = 4096;

/// Writable user page: at fork the child's copy is COW-protected (read-only),
/// so the child's first store must fault, get a private copy, and leave the
/// parent's view untouched.
static mut PAGE: [u8; PAGE_LEN] = [0xAA; PAGE_LEN];

fn page_matches(byte: u8) -> bool {
    unsafe {
        let p = core::ptr::addr_of!(PAGE) as *const u8;
        (0..PAGE_LEN).all(|i| core::ptr::read_volatile(p.add(i)) == byte)
    }
}

fn page_fill(byte: u8) {
    unsafe {
        let p = core::ptr::addr_of_mut!(PAGE) as *mut u8;
        for i in 0..PAGE_LEN {
            core::ptr::write_volatile(p.add(i), byte);
        }
    }
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    if !page_matches(0xAA) {
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
        // Child: the first store to PAGE hits the COW fault; the page must
        // become a private writable copy filled with our pattern.
        page_fill(0xBB);
        let ok = page_matches(0xBB);
        if ok {
            userland::write_out(b"forktest: child wrote 0xBB, verified\n");
            0
        } else {
            userland::write_err(b"forktest: child write verify FAILED\n");
            2
        }
    } else {
        // Parent: wait for the child's writes to land, then verify our view
        // of the shared page was not modified by the child (COW isolation).
        // A non-zero status (or a signal in the low byte) means the child
        // died instead of resolving its COW fault — fail loudly.
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
            userland::write_err(b" (COW fault not resolved)\n");
            return 3;
        }
        if !page_matches(0xAA) {
            userland::write_err(b"forktest: parent view corrupted by child (COW broken)\n");
            return 4;
        }
        // Parent's page was never COW'd — its own write goes straight through.
        page_fill(0xCC);
        let ok = page_matches(0xCC);
        if ok {
            userland::write_out(b"forktest: parent isolated, wrote 0xCC, verified\n");
            0
        } else {
            userland::write_err(b"forktest: parent write verify FAILED\n");
            5
        }
    }
}
