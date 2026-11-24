//! The guest's `std`-thread fixture, staged as `/usr/bin/threadprobe`.
//!
//! `rustc` runs a whole compilation on a spawned thread
//! (`rustc_interface::util::run_in_thread_with_globals`), and that thread's first
//! acts are to build its own TLS block, allocate from the heap, and read the
//! sysroot through VFS. `threadtest` covers `minix_rt::thread_create` directly,
//! with a 16 KiB stack, no thread pointer and no file I/O, so none of that path
//! was exercised before this probe.
//!
//! The probe prints the thread-local slot each thread sees. A worker that shared
//! the main thread's block would leave the main thread reading the worker's
//! value, which the final comparison turns into a failure instead of a number
//! that happens to look plausible.

use std::cell::{Cell, RefCell};

thread_local! {
    static SLOT: Cell<u32> = const { Cell::new(0) };
    static OWNED: RefCell<Option<Noisy>> = const { RefCell::new(None) };
}

/// A thread-local value that has to be dropped at thread exit, which is what
/// puts the standard library's per-thread destructor list in the path.
struct Noisy(&'static str);

impl Drop for Noisy {
    fn drop(&mut self) {
        println!("probe: drop {}", self.0);
    }
}

fn main() {
    SLOT.with(|slot| slot.set(1));
    OWNED.with(|owned| *owned.borrow_mut() = Some(Noisy("main")));
    println!("probe: main slot={}", SLOT.with(Cell::get));

    // One VFS round trip on the main thread first: a thread's `p_getfrom_e` keeps
    // naming the server it last received from, and the reply matcher reads it when
    // the same process is also waiting on another thread's behalf.
    println!("probe: main metadata {}", std::fs::metadata("/usr").is_ok());

    // `rustc` spawns its session thread with an 8 MiB stack
    // (`rustc_interface::util`'s stack-size default), which is far more than
    // `std`'s minimum. If a thread with that stack cannot start, no compiler
    // invocation gets past spawning it.
    let big = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| 7u32)
        .expect("an 8 MiB-stack thread did not start");
    println!(
        "probe: big stack returned {}",
        big.join().expect("the big thread panicked")
    );

    let worker = std::thread::spawn(|| {
        SLOT.with(|slot| slot.set(2));
        OWNED.with(|owned| *owned.borrow_mut() = Some(Noisy("worker")));
        println!("probe: worker slot={}", SLOT.with(Cell::get));

        println!(
            "probe: read_dir={:?}",
            std::fs::read_dir("/usr/lib/rustlib").map(|dir| dir.count())
        );
        println!(
            "probe: read={:?}",
            std::fs::read_to_string("/usr/src/hello.rs").map(|text| text.len())
        );
        // What `Target::search` does for a tuple that is not in the sysroot.
        println!(
            "probe: missing={}",
            std::fs::metadata("/usr/lib/rustlib/wasm32-wasip2/target.json").is_err()
        );
        println!(
            "probe: canonicalize={:?}",
            std::fs::canonicalize("/usr/bin/rustc").map(|path| path.display().to_string())
        );

        // A directory with more entries than one MFS reply holds. MFS replies
        // through a 1024-byte buffer, so roughly 28 of these long rlib names fit;
        // each further reply has to resume where the previous one stopped. The
        // walk used to re-read the same block with a full buffer for ever, which
        // is what `rustc`'s sysroot search does first.
        let lib = "/usr/lib/rustlib/x86_64-pc-minix/lib";
        let entries = std::fs::read_dir(lib).map(|dir| dir.count());
        println!("probe: sysroot lib entries={entries:?}");
        assert!(
            matches!(entries, Ok(count) if count > 28),
            "walking {lib} did not pass the end of the first reply: {entries:?}"
        );

        // The compiler reads the sysroot again and again. Hammer the same path so
        // a reply that is only occasionally misrouted shows up here, where the
        // loop counter says how far it got, instead of inside rustc.
        for i in 0..200u32 {
            if std::fs::metadata("/usr/lib/rustlib").is_err() {
                println!("probe: metadata failed at {i}");
                return 1;
            }
            if i % 50 == 0 {
                println!("probe: iter {i}");
            }
        }
        println!("probe: loop done");

        41
    });

    println!(
        "probe: worker returned {}",
        worker.join().expect("the worker panicked")
    );

    let after = SLOT.with(Cell::get);
    assert_eq!(after, 1, "the worker wrote into the main thread's TLS block");
    println!("probe: main slot={after}");
    println!("probe: ok");
}
