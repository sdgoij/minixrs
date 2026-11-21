//! Measure whether two processes mapping the same shared object share its
//! frames.
//!
//! Phase 5 of `.notes/dynamic_linking.md`. VM's file page cache is what makes a
//! read-only DSO page one physical frame for every process that maps it, and
//! that only holds if the loader maps the object's read-only segments
//! *read-only*: a loader that maps them writable so it can patch them in place
//! gets a private copy of every page. The doc's gate asks for a measured
//! assertion rather than a comment, so this measures: boot the dynamic-linking
//! image, run `/bin/dynclib hold | /bin/dynclib hold` (the shell's own fork and
//! exec, so two lives run the same dynamic image with `libc.so` mapped by the
//! loader), and walk *both* processes' page tables from outside the guest,
//! comparing the physical frames behind the object's read-only pages.
//!
//! Both lives start together, which is the hard case and the one the shell,
//! `init` and any build job produce. VM joins a fill already in flight, so the
//! second process waits for the first's page and maps that frame rather than
//! filling a private copy (`vm/vfs_request.rs::park_page` and `release_parked`).
//!
//! Only read-only pages are compared. Each mapping's writable `.data`/`.got` is
//! *expected* to be private (that is where the relocations go), so counting
//! those as failures would fail every run.
//!
//! Why not ask the guest: the loader is the thing under test, so a guest-side
//! measurement would have the subject measure itself; and only the kernel knows
//! a virtual address's frame. The guest's part is only to *stay alive* (the
//! `hold` mode) while the host reads the page tables out of guest physical
//! memory.
//!
//! Self-validating on purpose: nothing is identified by process name, and the
//! kernel's `Proc` layout is read out of the image being booted rather than
//! hardcoded. A slot is a candidate only when it is not free *and* its first page in
//! the window holds an ELF header, which is what excludes an unrelated mapping that
//! happens to share the address (`fb`'s heap did). The run fails unless exactly two
//! are found *and* the same walk first sees the program text that several processes
//! share. A wrong table address or stride therefore cannot pass quietly: it finds
//! nothing, and says so.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{command_from, secs};
use crate::guest::Guest;
use crate::qmp::Qmp;
use crate::toolchain;

const QMP_PORT: u16 = 4444;
const SLOTS: usize = 256 + 5;
/// Walk the object's first 256 KiB: the loader's mapping starts at `dso_base`
/// and no DSO here is larger, so a present page in this range is one of the
/// object's.
const DSO_WINDOW: u64 = 0x40000;
/// Pages the two processes must *both* have faulted in, to compare.
const MIN_COMPARED: usize = 8;
/// Every minix program is linked at its arch's base, so a walk that finds nobody
/// there is a broken walk rather than a system with nothing in it.
const TEXT_WINDOW: u64 = 0x4000;
const MIN_TEXT_MAPPERS: usize = 5;
const PROMPT: &[u8] = b"# ";
const HOLD_MARKER: &[u8] = b"dynhold";
/// How many processes have to have the object mapped for the comparison to mean
/// anything: the two sides of the pipeline.
const NEED_MAPPERS: usize = 2;
/// `RtsFlags::SLOT_FREE`. An exited slot keeps the flags it died with, so the
/// bit, not equality with zero, is the test for a slot that still means a
/// process (`crates/kernel/src/proc.rs`).
const SLOT_FREE: u32 = 0x00001;
/// `offset_of(Proc, p_name)` and `offset_of(Proc, p_endpoint)`, both relative to
/// `p_seg`: they are diagnostic only, and a wrong number shows a garbled name,
/// never a false pass (`crates/kernel/src/proc.rs`).
const NAME_OFFSET: u64 = 240;
const ENDPOINT_OFFSET: u64 = 256;
const NAME_LEN: usize = 16;

/// One arch's page-table walk and the image it boots.
struct Paging {
    kernel_elf: PathBuf,
    image: PathBuf,
    /// The loader's base for its first object (`crates/ldso/src/layout.rs`), per
    /// arch: it moves with the user base.
    dso_base: u64,
    /// Every minix program is linked at its arch's base (the target spec).
    text_base: u64,
    /// The index shifts of the levels above the leaf table, root first; the
    /// leaf's own entry is index `(va >> 12) & 0x1FF` in every arch here.
    shifts: &'static [u32],
    pg_present: u64,
    pg_huge: u64,
    pg_writable: u64,
    frame: fn(u64) -> u64,
    /// `offset_of(Proc, p_seg)`. Only x86_64 publishes it (through
    /// `FPU_STATE_OFF`), so the other arches carry the number the kernel
    /// assembly hardcodes (crates/kernel/src/proc.rs).
    p_seg_offset: Option<u64>,
}

fn arches(root: &Path) -> Vec<(&'static str, Paging)> {
    vec![
        (
            "x86",
            Paging {
                kernel_elf: root.join("target/x86_64-pc-minix/release/kernel-boot"),
                image: root.join("target/images/x86_64-pc-minix/minix-x86.elf"),
                // The x86 user base is 64 MiB, above the kernel image
                // (`.notes/physmap.md` P4), so its objects start at 80 MiB and
                // its programs link at 64 MiB.
                dso_base: 0x0500_0000,
                text_base: 0x0400_0000,
                shifts: &[39, 30, 21],
                pg_present: 0x001,
                pg_huge: 0x080,
                pg_writable: 0x002,
                frame: |entry| entry & 0x000F_FFFF_FFFF_F000,
                p_seg_offset: None,
            },
        ),
        (
            "riscv64",
            Paging {
                kernel_elf: root.join("target/riscv64gc-unknown-minix/release/kernel-boot-riscv64"),
                image: root.join("target/riscv64gc-unknown-minix/release/kernel-boot-riscv64"),
                dso_base: 0x0200_0000,
                text_base: 0x0100_0000,
                shifts: &[30, 21],
                pg_present: 0x001,
                pg_huge: 0x00A,
                pg_writable: 0x004,
                frame: |entry| ((entry >> 10) & 0x000F_FFFF_FFFF) << 12,
                p_seg_offset: Some(264),
            },
        ),
    ]
}

pub fn run(root: &Path, arch: &str, memory: &str, dynclib: &str) -> io::Result<bool> {
    let Some((_, paging)) = arches(root).into_iter().find(|(name, _)| *name == arch) else {
        let known: Vec<&str> = arches(root).into_iter().map(|(name, _)| name).collect();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --arch {arch} (have {})", known.join(", ")),
        ));
    };
    if !paging.image.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no image at {} - build it first", paging.image.display()),
        ));
    }

    let mut guest = Guest::spawn(&mut qemu(&paging, arch, memory))?;
    if !guest.wait_for(PROMPT, secs(60)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }

    let mut mem = QmpMemory {
        qmp: Qmp::connect(QMP_PORT)?,
        path: root.join("target/dso_share.bin"),
    };

    let place = layout(&paging, root, &mut mem)?;
    println!(
        "dso-share probe ({arch}): proc table 0x{:x}, slot stride {}, p_cr3 at +{}",
        place.0, place.1, place.2
    );

    let hold = format!("{dynclib} hold | {dynclib} hold");
    if !send_command(&mut guest, &hold) {
        return Ok(fail(
            &guest,
            &format!("the guest never echoed the whole of '{hold}'"),
        ));
    }

    // One marker per process of the pipeline, each printed after the loader has
    // run and `crt0` has entered `main`.
    let deadline = Instant::now() + secs(30);
    while guest.count(HOLD_MARKER) < NEED_MAPPERS {
        if Instant::now() >= deadline {
            return Ok(fail(
                &guest,
                &format!(
                    "fewer than {NEED_MAPPERS} '{}' lines - the pipeline never started",
                    String::from_utf8_lossy(HOLD_MARKER)
                ),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(500));

    let records = slot_records(&place, &mut mem)?;
    let roots: Vec<u64> = records.iter().map(|record| record.root).collect();
    let live = records.iter().filter(|record| record.root != 0).count();
    println!("{live} slot(s) hold a page-table root");

    // Before trusting the walk, check it can see something known: every minix
    // program is linked at `text_base`, so a live system has several processes
    // there. A walk that finds none is a broken instrument, not a finding.
    let text = mappers(&paging, &mut mem, &roots, paging.text_base, TEXT_WINDOW)?;
    if text.len() < MIN_TEXT_MAPPERS {
        return Ok(fail(
            &guest,
            &format!(
                "only {} process(es) map the program text at 0x{:x} - the page-table walk \
                 is not working, so it cannot be believed about the object either",
                text.len(),
                paging.text_base
            ),
        ));
    }

    let mapper = object_mappers(&paging, &mut mem, &roots, paging.dso_base, DSO_WINDOW)?;
    if mapper.len() != NEED_MAPPERS {
        let slots: Vec<String> = mapper
            .keys()
            .map(|slot| {
                let record = &records[*slot];
                format!("{} (slot {slot}, ep 0x{:x})", record.name, record.endpoint)
            })
            .collect();
        return Ok(fail(
            &guest,
            &format!(
                "{} process(es) map the object at 0x{:x}, expected {NEED_MAPPERS}: {}",
                mapper.len(),
                paging.dso_base,
                slots.join(", ")
            ),
        ));
    }

    let mut entries = mapper.iter();
    let (&sa, a) = entries.next().expect("two mappers were just counted");
    let (&sb, b) = entries.next().expect("two mappers were just counted");

    // The control, without which a "differ" verdict cannot be read: the same two
    // processes' own program text is mapped by exec, not by the loader.
    if let (Some(ta), Some(tb)) = (text.get(&sa), text.get(&sb)) {
        let common: Vec<u64> = ta.keys().filter(|va| tb.contains_key(va)).copied().collect();
        let mut ro = Vec::new();
        for va in common {
            if leaf(&paging, &mut mem, roots[sa], va)? & paging.pg_writable == 0 {
                ro.push(va);
            }
        }
        let same = ro
            .iter()
            .filter(|va| ta.get(*va) == tb.get(*va))
            .count();
        println!(
            "control: the same two processes share {same}/{} read-only program-text page(s) \
             at 0x{:x}",
            ro.len(),
            paging.text_base
        );
        if same != ro.len() {
            report_pages(
                &paging,
                &mut mem,
                &roots,
                View { slot: sa, frames: ta },
                View { slot: sb, frames: tb },
                &ro,
            )?;
        }
    }

    // Compare the read-only pages, and only those: the writable part of each
    // mapping is private by design.
    let common: Vec<u64> = a.keys().filter(|va| b.contains_key(va)).copied().collect();
    let mut rw = Vec::new();
    let mut ro = Vec::new();
    for va in common {
        if leaf(&paging, &mut mem, roots[sa], va)? & paging.pg_writable != 0 {
            rw.push(va);
        } else {
            ro.push(va);
        }
    }
    if ro.len() < MIN_COMPARED {
        return Ok(fail(
            &guest,
            &format!(
                "only {} read-only page(s) are present in both processes (need \
                 {MIN_COMPARED}); slot {sa} has {}, slot {sb} has {}",
                ro.len(),
                a.len(),
                b.len()
            ),
        ));
    }

    let differing: Vec<u64> = ro
        .iter()
        .filter(|va| a.get(*va) != b.get(*va))
        .copied()
        .collect();
    if !differing.is_empty() {
        let va = differing[0];
        let pa = leaf(&paging, &mut mem, roots[sa], va)?;
        let pb = leaf(&paging, &mut mem, roots[sb], va)?;
        println!(
            "FAIL dso-share: {} of {} read-only page(s) differ; first at 0x{va:x}: \
             frames 0x{:x} vs 0x{:x}, PTEs 0x{pa:x} (RW={}) vs 0x{pb:x} (RW={})",
            differing.len(),
            ro.len(),
            a.get(&va).copied().unwrap_or(0),
            b.get(&va).copied().unwrap_or(0),
            pa & paging.pg_writable != 0,
            pb & paging.pg_writable != 0
        );
        report_pages(
            &paging,
            &mut mem,
            &roots,
            View { slot: sa, frames: a },
            View { slot: sb, frames: b },
            &ro,
        )?;
        return Ok(false);
    }

    let frame = a.get(&ro[0]).copied().unwrap_or(0);
    println!(
        "PASS dso-share ({arch}): all {} read-only page(s) of the object at 0x{:x} are one \
         frame set in two processes (slots {sa} and {sb}; first page frame 0x{frame:x}); \
         {} writable page(s) are private, as intended",
        ro.len(),
        paging.dso_base,
        rw.len()
    );
    Ok(true)
}

/// A QEMU command for `arch`, with the process table readable over QMP.
fn qemu(paging: &Paging, arch: &str, memory: &str) -> Command {
    let program = if arch == "x86" {
        "qemu-system-x86_64"
    } else {
        "qemu-system-riscv64"
    };
    let qmp = format!("tcp:127.0.0.1:{QMP_PORT},server,nowait");
    let mut words = vec![program.to_string()];
    if arch == "x86" {
        words.extend(
            [
                "-nographic", "-monitor", "none", "-qmp", &qmp, "-m", memory, "-no-reboot",
                "-vga", "none", "-device", "bochs-display,id=fb0", "-netdev", "user,id=net0",
                "-device", "virtio-net-pci,disable-legacy=on,netdev=net0", "-device",
                "virtio-tablet-pci,display=fb0",
            ]
            .map(String::from),
        );
    } else {
        words.extend(
            [
                "-machine", "virt", "-m", memory, "-nographic", "-monitor", "none", "-qmp",
                &qmp, "-global", "virtio-mmio.force-legacy=off", "-netdev", "user,id=net0",
                "-device", "virtio-net-device,netdev=net0", "-device", "virtio-gpu-device",
                "-device", "virtio-keyboard-device",
            ]
            .map(String::from),
        );
    }
    let mut command = command_from(&words);
    command.arg("-kernel").arg(&paging.image);
    command
}

fn fail(guest: &Guest, message: &str) -> bool {
    // A probe that fails without the console is one that has to be re-run to be
    // understood.
    eprintln!("error: {message}");
    eprintln!("--- guest console (tail) ---");
    eprintln!("{}", guest.tail(2000));
    eprintln!("--- end ---");
    false
}

/// Type one command, paced, and check the guest echoed the whole of it.
///
/// The console drops what it is not ready for, and it does so *mid-line*, not
/// just at the tail. A truncated line still runs, as a shorter command, so
/// sending is not evidence that the command ran; the echo is, checked whole. One
/// byte per write is what a riscv64 console keeps, and it is safe on x86 too.
fn send_command(guest: &mut Guest, command: &str) -> bool {
    let echo = command.as_bytes();
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(400));
        let start = guest.transcript_len();
        if guest
            .send_paced(command, Duration::from_millis(20))
            .is_err()
        {
            return false;
        }
        // The command's own echo, not `"# " + command`: the prompt that precedes
        // it was printed before `start`, so a needle spanning the two is never
        // contiguous in the window.
        if guest.wait_for_after(echo, start, secs(10)) {
            return true;
        }
        // Something shorter ran. Let the shell come back before trying again, or
        // the retry lands in the same window.
        guest.wait_for_after(b"\n# ", start, secs(10));
    }
    false
}

/// Guest physical memory, read through QMP's `pmemsave`.
trait Physical {
    fn read(&mut self, phys: u64, size: u64) -> io::Result<Vec<u8>>;
}

struct QmpMemory {
    qmp: Qmp,
    path: PathBuf,
}

impl Physical for QmpMemory {
    fn read(&mut self, phys: u64, size: u64) -> io::Result<Vec<u8>> {
        self.qmp.pmemsave(phys, size, &self.path)?;
        fs::read(&self.path)
    }
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let slice = bytes.get(offset..offset + 8).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "a page-table read is short")
    })?;
    let mut buffer = [0u8; 8];
    buffer.copy_from_slice(slice);
    Ok(u64::from_le_bytes(buffer))
}

/// One page-table entry out of guest physical memory.
fn entry(mem: &mut impl Physical, table: u64, index: u64) -> io::Result<u64> {
    let bytes = mem.read(table + index * 8, 8)?;
    word(&bytes, 0)
}

/// Physical address of the leaf page table covering `base`, or `None`.
///
/// One table page per level above the leaf. A level whose entry is present but
/// is itself a leaf (a huge page) is not followed, because this walk only has
/// 4 KiB pages.
fn leaf_table(
    paging: &Paging,
    mem: &mut impl Physical,
    root: u64,
    base: u64,
) -> io::Result<Option<u64>> {
    let mut table = root;
    for shift in paging.shifts {
        let entry = entry(mem, table, (base >> shift) & 0x1FF)?;
        if entry & paging.pg_present == 0 || entry & paging.pg_huge != 0 {
            return Ok(None);
        }
        table = (paging.frame)(entry);
    }
    Ok(Some(table))
}

/// The raw 4 KiB page-table entry for `va` in `root`, or 0 when unmapped.
fn leaf(paging: &Paging, mem: &mut impl Physical, root: u64, va: u64) -> io::Result<u64> {
    let Some(table) = leaf_table(paging, mem, root, va)? else {
        return Ok(0);
    };
    entry(mem, table, (va >> 12) & 0x1FF)
}

/// Every present page of a mapping in this address space, as `va -> frame`.
///
/// The window lies inside one 2 MiB region, so the walk needs one table read per
/// process rather than one per page.
fn frames_in(
    paging: &Paging,
    mem: &mut impl Physical,
    root: u64,
    base: u64,
    window: u64,
) -> io::Result<BTreeMap<u64, u64>> {
    if (base >> 21) != ((base + window - 1) >> 21) {
        return Err(io::Error::other("the window must not span a 2 MiB region"));
    }
    let Some(table) = leaf_table(paging, mem, root, base)? else {
        return Ok(BTreeMap::new());
    };
    let page = mem.read(table, 4096)?;
    let mut found = BTreeMap::new();
    let mut offset = 0;
    while offset < window {
        let index = ((base + offset) >> 12) & 0x1FF;
        let entry = word(&page, index as usize * 8)?;
        if entry & paging.pg_present != 0 {
            found.insert(base + offset, (paging.frame)(entry));
        }
        offset += 4096;
    }
    Ok(found)
}

/// Every live slot that maps the shared object at `base`.
///
/// A process can have an unrelated mapping at the same virtual address: on this
/// tree `fb`'s heap landed on `dso_base`, and counting that as a third mapper
/// failed every run. A slot counts only when the first page of its window holds
/// an ELF header, which is what the loader maps the object from; nothing else a
/// program puts at that address starts with one.
fn object_mappers(
    paging: &Paging,
    mem: &mut impl Physical,
    roots: &[u64],
    base: u64,
    window: u64,
) -> io::Result<BTreeMap<usize, BTreeMap<u64, u64>>> {
    let mut found = BTreeMap::new();
    for (slot, root) in roots.iter().enumerate() {
        if *root == 0 {
            continue;
        }
        let frames = frames_in(paging, mem, *root, base, window)?;
        let Some(frame) = frames.get(&base) else {
            continue;
        };
        let header = mem.read(*frame, 4)?;
        if header.as_slice() != b"\x7fELF" {
            continue;
        }
        found.insert(slot, frames);
    }
    Ok(found)
}

/// Every live slot that maps `base`, as `slot -> {va: frame}`.
fn mappers(
    paging: &Paging,
    mem: &mut impl Physical,
    roots: &[u64],
    base: u64,
    window: u64,
) -> io::Result<BTreeMap<usize, BTreeMap<u64, u64>>> {
    let mut found = BTreeMap::new();
    for (slot, root) in roots.iter().enumerate() {
        if *root == 0 {
            continue;
        }
        let frames = frames_in(paging, mem, *root, base, window)?;
        if !frames.is_empty() {
            found.insert(slot, frames);
        }
    }
    Ok(found)
}

/// Address and size of each kernel symbol, read out of the image.
fn symbol_places(
    root: &Path,
    names: &[&str],
    elf: &Path,
) -> io::Result<BTreeMap<String, (u64, u64)>> {
    let nm = toolchain::find_nm(root).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no llvm-nm to read the image's symbols (set MINIXRS_LLVM_NM, or install LLVM)",
        )
    })?;
    let output = Command::new(&nm).arg("--print-size").arg(elf).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} failed on {}",
            nm.display(),
            elf.display()
        )));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut placed = BTreeMap::new();
    for line in listing.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        for name in names {
            if fields[fields.len() - 1].ends_with(name) {
                let address = u64::from_str_radix(fields[0], 16).map_err(invalid)?;
                let size = u64::from_str_radix(fields[1], 16).map_err(invalid)?;
                placed.insert((*name).to_string(), (address, size));
            }
        }
    }
    for name in names {
        if !placed.contains_key(*name) {
            return Err(io::Error::other(format!(
                "{} has no symbol for {name}",
                elf.display()
            )));
        }
    }
    Ok(placed)
}

fn invalid(error: std::num::ParseIntError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// `(table, stride, p_cr3)` for the running kernel.
///
/// `p_cr3` is `offset_of(Proc, p_seg)`: `SegFrame` starts with `p_cr3`
/// (`#[repr(C)]`, crates/kernel/src/proc.rs). Only x86_64 publishes that offset,
/// through `FPU_STATE_OFF` (which is `p_seg` plus `fpu_state`'s own offset of
/// 16), so it is read there and taken from [`Paging`] elsewhere. A wrong answer
/// here cannot pass quietly: the walk has to see the program text before it
/// believes anything it reads.
fn layout(
    paging: &Paging,
    root: &Path,
    mem: &mut impl Physical,
) -> io::Result<(u64, u64, u64)> {
    if !paging.kernel_elf.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no kernel image at {} - build it first", paging.kernel_elf.display()),
        ));
    }
    let mut names = vec!["PROC_TABLE_ALIGNED"];
    if paging.p_seg_offset.is_none() {
        names.push("FPU_STATE_OFF");
    }
    let places = symbol_places(root, &names, &paging.kernel_elf)?;

    let (table, table_size) = *places
        .get("PROC_TABLE_ALIGNED")
        .ok_or_else(|| io::Error::other("the image has no PROC_TABLE_ALIGNED"))?;
    let stride = table_size / SLOTS as u64;
    let p_seg = match paging.p_seg_offset {
        Some(offset) => offset,
        None => {
            let (address, _) = *places
                .get("FPU_STATE_OFF")
                .ok_or_else(|| io::Error::other("the image has no FPU_STATE_OFF"))?;
            let bytes = mem.read(address, 8)?;
            word(&bytes, 0)?.saturating_sub(16)
        }
    };
    if stride == 0 || p_seg < 16 || p_seg >= stride {
        return Err(io::Error::other(format!(
            "the kernel image's layout does not make sense (table size {table_size}, \
             p_cr3 offset {p_seg})"
        )));
    }
    Ok((table, stride, p_seg))
}

/// One slot's page-table root, name and endpoint.
struct Slot {
    root: u64,
    name: String,
    endpoint: i32,
}

/// Every slot's record, out of one dump of the whole table.
///
/// Taken *after* the pipeline has started: a slot's root is zero while the slot
/// is free, but an exited slot may keep a nonzero root that still aliases the
/// pages it used to map (`crates/kernel/src/proc.rs`), so the free bit is what
/// excludes it.
fn slot_records(place: &(u64, u64, u64), mem: &mut impl Physical) -> io::Result<Vec<Slot>> {
    let (table, stride, p_seg) = *place;
    let rts = rts_offset(p_seg);
    let dump = mem.read(table, SLOTS as u64 * stride)?;
    let mut records = Vec::with_capacity(SLOTS);
    for slot in 0..SLOTS {
        let base = slot * stride as usize;
        let flags = word(&dump, base + rts as usize)? as u32;
        let root = word(&dump, base + p_seg as usize)?;
        let name_at = base + p_seg as usize + NAME_OFFSET as usize;
        let name = dump
            .get(name_at..name_at + NAME_LEN)
            .map(|bytes| {
                let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            })
            .unwrap_or_default();
        let endpoint = word(&dump, base + p_seg as usize + ENDPOINT_OFFSET as usize)? as i32;
        records.push(Slot {
            root: if flags & SLOT_FREE == 0 { root } else { 0 },
            name,
            endpoint,
        });
    }
    Ok(records)
}

/// `offset_of(Proc, p_rts_flags)` from `offset_of(Proc, p_seg)`.
///
/// `SegFrame` is 32 bytes on every arch here, and `p_nr`, four bytes of padding
/// and the `p_priv` pointer (16 bytes) follow it, so the runtime flags are 48
/// bytes past `p_seg` (`crates/kernel/src/proc.rs`).
fn rts_offset(p_seg: u64) -> u64 {
    p_seg + 48
}

/// One side of a page comparison: the slot and its `va -> frame` map.
struct View<'a> {
    slot: usize,
    frames: &'a BTreeMap<u64, u64>,
}

/// Per-page frames and PTE flags, which is what makes a failure diagnosable.
///
/// A read-only PTE that is still private means the page was never shared (a
/// cache miss); a writable one means the region was mapped writable, which is a
/// mapping discipline problem instead.
fn report_pages(
    paging: &Paging,
    mem: &mut impl Physical,
    roots: &[u64],
    first: View<'_>,
    second: View<'_>,
    vas: &[u64],
) -> io::Result<()> {
    for va in vas {
        let ea = leaf(paging, mem, roots[first.slot], *va)?;
        let eb = leaf(paging, mem, roots[second.slot], *va)?;
        println!(
            "  0x{va:x}: {} frames 0x{:x}/0x{:x} RW={}/{}",
            if first.frames.get(va) == second.frames.get(va) {
                "SAME"
            } else {
                "diff"
            },
            first.frames.get(va).copied().unwrap_or(0),
            second.frames.get(va).copied().unwrap_or(0),
            ea & paging.pg_writable != 0,
            eb & paging.pg_writable != 0
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guest memory backed by a flat byte vector, for a hand-built page table.
    struct Fake(Vec<u8>);

    impl Physical for Fake {
        fn read(&mut self, phys: u64, size: u64) -> io::Result<Vec<u8>> {
            let start = phys as usize;
            let end = start + size as usize;
            self.0
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| io::Error::other("a read past the fake memory"))
        }
    }

    fn put(memory: &mut [u8], table: u64, index: u64, value: u64) {
        let at = table as usize + index as usize * 8;
        memory[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn x86() -> (Fake, &'static Paging) {
        static PAGING: Paging = Paging {
            kernel_elf: PathBuf::new(),
            image: PathBuf::new(),
            dso_base: 0x0500_0000,
            text_base: 0x0400_0000,
            shifts: &[39, 30, 21],
            pg_present: 0x001,
            pg_huge: 0x080,
            pg_writable: 0x002,
            frame: |entry| entry & 0x000F_FFFF_FFFF_F000,
            p_seg_offset: None,
        };
        (Fake(vec![0u8; 0x8000]), &PAGING)
    }

    #[test]
    fn the_x86_walk_finds_a_mapped_page_and_its_frame() {
        let (mut fake, paging) = x86();
        let base = paging.dso_base;
        let (root, pdpt, pd, pt) = (0x1000, 0x2000, 0x3000, 0x4000);
        put(&mut fake.0, root, (base >> 39) & 0x1FF, pdpt | 0x003);
        put(&mut fake.0, pdpt, (base >> 30) & 0x1FF, pd | 0x003);
        put(&mut fake.0, pd, (base >> 21) & 0x1FF, pt | 0x003);
        put(&mut fake.0, pt, (base >> 12) & 0x1FF, 0x9000 | 0x001);

        let frames = frames_in(paging, &mut fake, root, base, DSO_WINDOW).unwrap();
        assert_eq!(frames.get(&base), Some(&0x9000));
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn a_huge_page_is_not_walked_as_a_table() {
        let (mut fake, paging) = x86();
        let base = paging.dso_base;
        let (root, pdpt, pd) = (0x1000, 0x2000, 0x3000);
        put(&mut fake.0, root, (base >> 39) & 0x1FF, pdpt | 0x003);
        put(&mut fake.0, pdpt, (base >> 30) & 0x1FF, pd | 0x003);
        // Present, and the x86 huge-page bit, so there is no leaf table below it.
        put(&mut fake.0, pd, (base >> 21) & 0x1FF, 0x2000_0000 | 0x081);
        assert_eq!(
            leaf_table(paging, &mut fake, root, base).unwrap(),
            None
        );
    }

    #[test]
    fn the_riscv_walk_masks_the_frame_the_way_sv39_does() {
        static PAGING: Paging = Paging {
            kernel_elf: PathBuf::new(),
            image: PathBuf::new(),
            dso_base: 0x0200_0000,
            text_base: 0x0100_0000,
            shifts: &[30, 21],
            pg_present: 0x001,
            pg_huge: 0x00A,
            pg_writable: 0x004,
            frame: |entry| ((entry >> 10) & 0x000F_FFFF_FFFF) << 12,
            p_seg_offset: Some(264),
        };
        let mut fake = Fake(vec![0u8; 0x8000]);
        let base = PAGING.dso_base;
        let (root, mid, leaf_table_page) = (0x1000, 0x2000, 0x3000);
        // SV39 carries a physical page in the PPN field (bits 10 and up), not as
        // a raw address, so a table pointer is `ppn << 10 | flags`.
        let pointer = |page: u64| ((page >> 12) << 10) | 0x001;
        put(&mut fake.0, root, (base >> 30) & 0x1FF, pointer(mid));
        put(&mut fake.0, mid, (base >> 21) & 0x1FF, pointer(leaf_table_page));
        // A 4 KiB leaf: PPN 0x9, read-only, present.
        put(
            &mut fake.0,
            leaf_table_page,
            (base >> 12) & 0x1FF,
            (0x9 << 10) | 0x001,
        );

        let frames = frames_in(&PAGING, &mut fake, root, base, DSO_WINDOW).unwrap();
        assert_eq!(frames.get(&base), Some(&0x9000));
        assert_eq!(
            leaf(&PAGING, &mut fake, root, base).unwrap() & PAGING.pg_writable,
            0
        );
    }

    #[test]
    fn the_runtime_flags_sit_48_bytes_past_the_segment_frame() {
        assert_eq!(rts_offset(256), 304);
        assert_eq!(rts_offset(264), 312);
    }

    #[test]
    fn a_symbol_listing_is_read_by_its_last_component() {
        let listing = "\
00000000002747a8 0000000000000008 B FPU_STATE_OFF
000000000023b380 0000000000039180 B _RNvNtCs2BL2bRRwVJi_6kernel5table18PROC_TABLE_ALIGNED
";
        let mut placed = BTreeMap::new();
        for line in listing.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 {
                continue;
            }
            for name in ["PROC_TABLE_ALIGNED", "FPU_STATE_OFF"] {
                if fields[fields.len() - 1].ends_with(name) {
                    placed.insert(
                        name.to_string(),
                        (
                            u64::from_str_radix(fields[0], 16).unwrap(),
                            u64::from_str_radix(fields[1], 16).unwrap(),
                        ),
                    );
                }
            }
        }
        assert_eq!(placed["FPU_STATE_OFF"].0, 0x2747a8);
        assert_eq!(placed["PROC_TABLE_ALIGNED"].1, 0x39180);
        assert_eq!(0x39180 / SLOTS as u64, 896);
    }
}
