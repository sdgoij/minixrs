//! The page-fault error code VM decodes, derived from a RISC-V fault.
//!
//! Ungated so the host suite covers it (`trap.rs`, which calls this, is compiled only
//! for the target): the code is a table, and the one thing that goes wrong in it — a
//! load that claims to be a write — is invisible until a process is killed for
//! reading a page it is allowed to read.
//!
//! The layout is x86_64's, because that is what VM and the kernel's fault gate
//! decode:
//!
//! - bit 0, `PRESENT`: the page was there, so this is a protection violation. VM reads
//!   this bit as "the access was a write" — it is what selects the write permission a
//!   fault is checked against, and it is one half of what makes a fault a
//!   copy-on-write candidate.
//! - bit 1, `WRITE`: the access was a write. VM reads this bit as "the page was
//!   present", and pairs it with bit 0 to arm the copy-on-write path.
//! - bit 2, `USER`: the fault was taken in user mode.
//! - bit 4, `INSTR`: an instruction fetch, not a load or a store.
//!
//! RISC-V states neither bit 0 nor VM's reading of it: `scause` says only load, store
//! or fetch. So bit 0 is set for a store and cleared for a load, which is the
//! distinction the two questions VM asks it are both about. A load that set it would
//! be refused on every region that is readable but not writable — each
//! `mmap(PROT_READ)` page, and every read-only segment of a shared object, since only
//! an `exec`'d image's non-executable segments are pre-faulted by VFS.

/// The page was present; VM reads it as "this access was a write".
pub const PRESENT: u32 = 0x01;
/// The access was a write; VM reads it as "the page was present".
pub const WRITE: u32 = 0x02;
/// The fault was taken in user mode.
pub const USER: u32 = 0x04;
/// The fault was an instruction fetch.
pub const INSTR: u32 = 0x10;

/// What the faulting instruction was doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Load,
    Store,
    Fetch,
}

/// The error code VM decodes for `access`, taken in user mode when `user`.
pub const fn page_fault_code(access: Access, user: bool) -> u32 {
    let mode = if user { USER } else { 0 };
    match access {
        // A load is the one access that says nothing beyond its mode: VM's two
        // questions about bit 0 are both about a store, and answering "yes" would
        // demand write permission for a read.
        Access::Load => mode,
        Access::Store => mode | PRESENT | WRITE,
        // Fetching needs neither write permission nor a present page: VM maps the
        // page from the region, as it does for a load.
        Access::Fetch => mode | INSTR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_load_claims_no_write() {
        let code = page_fault_code(Access::Load, true);
        assert_eq!(code, USER);
        assert_eq!(code & PRESENT, 0, "a load must not demand write permission");
        assert_eq!(code & WRITE, 0, "a load is not a write");
    }

    #[test]
    fn kernel_load_is_bare() {
        assert_eq!(page_fault_code(Access::Load, false), 0);
    }

    #[test]
    fn store_sets_present_and_write_so_cow_arms() {
        assert_eq!(page_fault_code(Access::Store, true), 0x07);
        assert_eq!(page_fault_code(Access::Store, false), 0x03);
    }

    #[test]
    fn fetch_is_neither_write_nor_present() {
        assert_eq!(page_fault_code(Access::Fetch, true), 0x14);
        assert_eq!(page_fault_code(Access::Fetch, false), 0x10);
    }

    #[test]
    fn only_a_store_claims_the_write_permission() {
        for user in [true, false] {
            assert_eq!(page_fault_code(Access::Load, user) & PRESENT, 0);
            assert_eq!(page_fault_code(Access::Fetch, user) & PRESENT, 0);
            assert_eq!(page_fault_code(Access::Store, user) & PRESENT, PRESENT);
        }
    }
}
