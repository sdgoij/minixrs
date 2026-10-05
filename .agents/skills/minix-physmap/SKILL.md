---
name: minix-physmap
description: Physical-memory traps in the MINIX/Rust kernel — the kernel runs on the calling process's page tables, so a physical address used as a pointer can be shadowed by a user mapping. Use when a kernel read or write touches a frame, a page table, a grant table or a device register; when turning a physical address (`pte_to_phys`, a frame mask, a `*_phys`/`*_pa` value) into a pointer; when adding a kernel mapping; or when a copy lands in the wrong process or a reply comes back as zeros. Covers `phys_to_virt`/`frame_ptr`/`table_ptr`/`phys_ptr`, `identity_map_top()`, `MAX_USER_ADDRESS` and `tools/check-physmap.py`.
---

# Physical memory and the physmap

The invariant:

> A kernel access to a physical frame is sound only if the virtual address used for it **cannot be a
> user mapping in any address space the kernel may run on.**

The kernel runs on the *caller's* page tables — that is what makes `delivermsg`, `read_from_proc`
and `copy_from_user` work. So any VA the kernel picks for a mapping of physical memory is a VA some
process could also be given. Deref a physical address at `VA == PA` and what you read is whatever
the running process has mapped there: for a large server, its own image. `KNOWN_ISSUES.md` item 38
is this bug measured — `copy_from_user` read a frame at `VA == PA` inside the sender's own image and
the server answered zeros, with no fault anywhere.

## Converting a physical address to a pointer

Never cast a physical address to a pointer. Convert it:

- `phys_to_virt(pa)` (re-exported by `crate::hal`) is the only conversion; `virt_to_phys(va)` is its
  inverse.
- The seams built on it are `table_ptr` (a page-table frame, `crates/kernel/src/pagetable.rs`),
  `frame_ptr` (a data frame) and `phys_ptr` (the arch boot/exec table builders, behind
  `arch_common::PhysAccess`).
- `pte_to_phys(..)` returns a number that *looks* like a pointer and was one for this port's whole
  history. Converting it is the rule — for page tables (P2b) and for frames (P3/P4).

`tools/check-physmap.py`, wired into `just check`, enforces this over `crates/kernel/src` and
`crates/arch-*/src`: it rejects a page-table-derived quantity (`pte_to_phys`/`pte_frame`/a frame
mask) or a `*_phys`/`*_pa`/`pa`/`phys` binding cast to a pointer unless the line converts through
`phys_to_virt`/`frame_ptr`/`table_ptr`/`phys_ptr`. It skips each file's `#[cfg(test)] mod tests`
tail, because a host fixture has no physmap. An exception states itself on the line with
`physmap-ok: <reason>`; the eight that remain are the pointer seams' host arms, `phys_ptr`'s
`PhysAccess::Identity` arm (the boot builder, which runs before the window) and
`table_ptr`/`frame_ptr`'s own `#[cfg(test)]` arms. Do not add another without a reason of that kind
— a bare cast is the bug the checker exists for.

## The identity map is not the physmap

- The **physmap** is the mapping of physical memory at an address above every user window: x86-64
  `0xFFFF_8000_0000_0000`, riscv64 `0xFFFF_FFC0_0000_0000`, aarch64 `0x8_0000_0000` (low, because
  `EPD1` disables TTBR1). It is installed in every address space, supervisor-only, non-executable,
  and covers RAM and device MMIO. Use it for every frame deref.
- The **identity map** is a small, named exception: supervisor-only, and in every per-process table
  it stops at `identity_map_top()` — the user base — so it is the kernel image and the low frames
  and nothing else. It is *not* a way to reach physical memory, and no user mapping may be placed
  where it maps.
- The **boot tables keep the broad 0..32 GiB map deliberately**: `pre_init`, the trampoline and
  `kmain @ 0x200000` reach physical memory before a physmap exists, and the top-down boot
  allocator's own page has to be reachable at its physical address. Early-boot frame writes and MMIO
  are the boot path's exception — do not "fix" them, and do not copy the pattern into code that runs
  after the window is installed.
- A kernel-only mapping's VA must be outside every user window. The per-arch physmap base is
  asserted against `MAX_USER_ADDRESS` at compile time; do not add one below it.

## Device MMIO is a physical address too

The LAPIC and I/O APIC registers (`apic.rs`, `0xFEE0_0000`/`0xFEC0_0000`) go through the physmap; so
do AArch64's PL011/GIC and RISC-V's UART, PLIC and test finisher. A device reached at its physical
address *after* paging is on is invisible until something faults — AArch64's boot console cost a
`-d int` session (`ESR 0x96000005`, `FAR 0x9000018`) to find, and RISC-V's low-1 GiB devices are the
same shape. Those reach registers raw *before* the MMU is on and through the physmap after, on a
`boot_cr3() == 0` test; with paging off there is no translation, so the physmap address would be
invalid.

On x86 a PCI BAR is not dereferenced by the kernel at all: `VM_MAP_PHYS`/`do_map_phys` gives it a
`VR_DIRECT` window in the target's own region space (P5) and drivers read that mapping. Do not
reintroduce `vaddr = phys`; the reference's `do_map_phys` allocates the region's vaddr for the same
reason.

## Review checklist

- Does a kernel read/write touch a **frame**? Convert it (`phys_to_virt`/`frame_ptr`), don't cast it.
- Does it reach a **page table**? `table_ptr`, and keep the table's physical address separate from
  the pointer you write through.
- Does it reach a **device register**? Through the physmap — and check whether it runs before the
  window is installed.
- Did you add a **kernel mapping**? Its VA must be outside every user window.
- Is this the **boot path**? The identity map is the deliberate exception there; leave it and don't
  carry it forward.
- Did you add `physmap-ok:`? It needs a reason of the same kind as the existing eight.

## Where this is pinned

- `PHYSMAP.md` — the invariant, the per-stage plan (P1–P6) and the audit of every deref site.
- `tools/check-physmap.py` — the static rule; run by `just check`.
- `crates/kernel/src/pagetable.rs` (`table_ptr`, `frame_ptr`) and each arch's `vmparam.rs`
  (`phys_to_virt`, `identity_map_top`, the compile-time pins).
- Boot tests: `test_walk_through_the_physmap` (P2c) and
  `test_user_and_kernel_mappings_do_not_overlap` (P4/P6).
- `KNOWN_ISSUES.md` item 38 — the measured failure.

Related: the `minix-kernel-boundary` skill covers choosing the right *address space* (`boot_cr3() ==
0`, `kernel_to_proc`); this one covers the address a physical frame is reached *through*.
