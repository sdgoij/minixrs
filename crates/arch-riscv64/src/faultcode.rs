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
//! - bit 0, `PRESENT`: the page was there, so this is a protection violation. RISC-V
//!   does not report this (`scause` says only load, store or fetch), so the table
//!   claims it for a store and for nothing else: together with bit 1 it is what arms
//!   VM's copy-on-write path.
//! - bit 1, `WRITE`: the access was a write. VM reads it as the permission a fault has
//!   to be granted.
//! - bit 2, `USER`: the fault was taken in user mode.
//! - bit 4, `INSTR`: an instruction fetch, not a load or a store.
//!
//! A load or a fetch that claimed bit 0 would be taken for a copy-on-write candidate
//! and reach VM's demand path with write permission demanded, which refuses every
//! region that is readable but not writable — each `mmap(PROT_READ)` page, and every
//! read-only segment of a shared object, since only an `exec`'d image's non-executable
//! segments are pre-faulted by VFS.

/// The page was present; claimed only for a store, which is the access that can be a
/// copy-on-write candidate.
pub const PRESENT: u32 = 0x01;
/// The access was a write.
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
        // A load is the one access that says nothing beyond its mode: it is not a
        // store, so it claims neither the write bit nor the present bit that arms
        // copy-on-write.
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
        assert_eq!(
            code & PRESENT,
            0,
            "a load must not claim the page was present"
        );
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
