# PHYSMAP.md — a physmap the user window cannot shadow

Status: **P1, P2a, P2b and P2c landed (2026-09-29); P3–P6 are design.** This scopes the fix for the class that
`KNOWN_ISSUES.md` item 38's B exposed: the kernel dereferences *physical* addresses as pointers,
and the virtual address it does that at is one a user window can also occupy.

It is not a new feature. It is the invariant the port has been relying on without stating it, plus
the one mapping that makes the invariant hold by construction. It is also a prerequisite for
Phase 3 (`WAYLAND.md` §6.10): `do_map_phys` maps a device BAR at its *physical* address today,
which is the same hazard wearing a driver's hat.

---

## 1. The invariant

> A kernel access to a physical frame is sound only if the virtual address used for it **cannot be
> a user mapping in any address space the kernel may run on.**

The kernel runs on the calling process's page tables (that is what makes `delivermsg`,
`read_from_proc` and now `copy_from_user` work at all), so "the address space the kernel is
running on" is a user's. That is the whole difficulty: any VA the kernel picks for a mapping of
physical memory is a VA some process could also be given.

Two ways to satisfy the invariant:

1. **Never dereference a physical address.** Reach another address space the way `read_from_proc`
   does — switch to its CR3 and use the *virtual* address the owner already has — or map the frame
   into a scratch window first.
2. **Dereference it at a virtual address no user window can occupy.** Then the mapping is the
   kernel's alone, in every address space, and a bare `phys`→pointer convention is safe again.

The port's mapping of physical memory — the 0..32 GiB identity map — shares addresses with the
user window, so it is not that second address. P1 and P2a added the mapping that is (the physmap,
present in every address space) and P2b routes every page-table access through it, so mechanism 2
now holds for the walk and map helpers. What is left of mechanism 2 are the *frame* derefs below
(P3's list), one of which is item 38's measured failure: `copy_from_user` read a frame through
`VA == PA`, and `VA == PA` was inside the sender's own image.

## 2. What we start with (measured, not proposed)

**The identity map.** `exec_create_root` (`arch-x86_64`) and `boot_create_page_table`
(`kernel-boot`) both deep-copy the boot 0..32 GiB map into every address space as **2 MiB pages**,
one PD per GiB, **supervisor-only** (`e &= !PG_U`; RISC-V copies the same map, AArch64 keeps a low
1 GiB block table). So the map is not a privilege hole — a *user* access to it faults. It is a
*hazard* only for ring 0, which is exactly why item 38 presents as a server answering nonsense
rather than as a crash.

**The user window**, per arch, as it stands: item 38's B, which moved the base to 64 MiB, was
reverted (`KNOWN_ISSUES.md`), so these are the older addresses again.

| | x86_64 | riscv64 | aarch64 |
|---|---|---|---|
| `MAX_USER_ADDRESS` | `0x0000_8000_0000_0000` (128 TiB) | `0x40_0000_0000` (256 GiB) | `kern_vaddr()` (user below `0x4000_0000`) |
| main program / DSOs / loader | `0x0100_0000` / `0x0200_0000` / `0x0400_0000` | same | same |
| stack | `0x0FE0_0000` | `0x8FE0_0000` | `0x3FC0_0000` |
| heap / mmap from | `0x3FE0_0000` / 4 GiB | `0x3FE0_0000` / 4 GiB | `0x2000_0000` / `0x3000_0000` |
| VM scratch | top of the user space | top | a dedicated gap |

**The kernel image** is at `0x200000..~0x2100000` (2 MiB..33 MiB) and is linked *at* its physical
address — the trampoline loads it there and `kmain @ 0x200000`. It must stay mapped in every
address space. Because it reaches 17 MiB *past* the 16 MiB user base, part of it shares an address
with every process's window: that overlap is item 38, and moving the base above the image (item
38's B, 64 MiB) is part of this plan rather than of P1.

**What already exists and is right.** The port has two of the three mechanisms the reference
uses, which is why this is a smaller change than it looks:

- **VA-based cross-address-space copies:** `delivermsg` (switch CR3, write the target's VA),
  `read_from_proc` (same, for the exec frame), and `copy_from_user` after item 38 (read the
  caller's VA, walk only to answer "is that page there").
- **A scratch window:** `vm_scratch_base` / `VM_NEXT_MAP_VA` (`servers/src/vm`), which maps a page
  into VM's own address space so it can reach one it has no mapping for. This is the reference's
  `freepdes` idea, already implemented for the one consumer that needed it.

**What is missing.** The third mechanism was a mapping of physical memory at an address the user
window cannot occupy, and a rule about when a physical address may become a pointer. P1 and P2a
supply the mapping, P2b supplies the rule for page tables, and the rest of the table below — frames,
grant tables, device BARs — is P3's.

**The deref sites (the audit).** These are the places a `phys_bytes` becomes a pointer:

| Site | What it reaches | Today |
|---|---|---|
| `pagetable.rs` `walk`/`map_page`/`unmap_page`/`clear_page` | every page table | **P2b**: `table_ptr` = `phys_to_virt` |
| `exec.rs` | the ELF walk | **P2b**: `table_ptr` = `phys_to_virt` |
| `vm.rs` (page-fault fill, COW/copy) | a frame being filled | `pte_to_phys(..) + offset` then deref |
| `grants.rs` `verify_grant` | the *granter's* grant table | `s_grant_pa + i * size` (a physical address, by design — the granter may not be the running process) |
| `system.rs` `do_map_phys` | a device BAR | maps it *identity*, user-accessible |
| `ipc.rs` `copy_from_user` | the sender's message | **fixed** (reads the VA) |
| `boot_init` / pre-`init` boot | the kernel, the archive | identity, before any user mapping exists — stays |
| boot test `on_kernel_tables` | the initramfs | a workaround for exactly this (A for item 38) |

One trap worth recording because it cost time: `syscall.rs`'s comment on the exec clear said
`exec_create_root` copies the identity map "with user access in the low window". It does not —
it strips `PG_U`. The clear is right, but its stated reason was wrong, and the wrong reason
("so the map is user-visible") is the opposite of the actual hazard (ring 0 is the only thing that
can see the map, and ring 0 is what gets shadowed). Comment corrected.

## 3. What other systems do

### 3.1 MINIX 3.3.0 — the reference, read from source

This is the most useful data point because it is *this* system's ancestor, and because it answers
the question directly rather than by convention.

- **The kernel is linked to run high.** `arch/i386/kernel.lds`:
  `_kern_phys_base = 0x00400000`, `_kern_vir_base = 0xF0400000`, with the comment
  *"map kernel high for max. user vir space"*, and `_kern_offset` between them. `pg_mapkernel()`
  installs that mapping as **4 MiB big pages**.
- **`vir2phys()` is an offset, not an identity:** `(phys_bytes) vir - _kern_offset`. (The early,
  pre-paging definition *is* identity — `pre_init.c` — which is the shape we still have.)
- **User virtual space is asserted to be below the kernel:** `pg_map()`, which is what builds user
  mappings, begins `assert(vaddr < kern_vir_start)`.
- **Physical memory is reached through a scratch window, not a physmap.** `createpde()` reserves a
  pool of **free PDEs** (`freepdes[]`, told to the kernel by VM) and installs the target mapping
  into one of them: for a *process*'s page it copies that process's PDE entry, for *physical*
  memory (`pr == NULL`) it builds a 4 MiB big-page PDE — and returns a *kernel linear address*
  "for actual use by `phys_copy` or memset". The window is mapped, used, and unmapped.
- **Page tables are referenced by kernel virtual address:** every process carries
  `p_seg.p_cr3_v`, a kernel pointer to its page directory. `createpde` writes page tables through
  `p_cr3_v[...]`; nothing walks a table by physical address.

So MINIX's answer is: *kernel high, user space below it, a bounded scratch window for everything
else, and no bare physical dereference anywhere.* It has no full physmap at all, which is the
cheapest version of the invariant but the most per-copy work.

### 3.2 The 64-bit systems with a direct map

| System | Kernel / physmap placement | How a phys deref is expressed |
|---|---|---|
| Linux x86-64 (4-level) | user `0..0x0000_7fff_ffff_ffff`; kernel text `0xffff_ffff_8000_0000`; direct map `PAGE_OFFSET = 0xffff_8880_0000_0000` (5-level: `0xff11_0000_0000_0000`); randomized by `CONFIG_RANDOMIZE_MEMORY` | `__va(x)` / `__pa(x)` only; page tables are reached with `__va(pmd_val(..))`, never a bare phys |
| Linux arm64 (48-bit) | user low half; `PAGE_OFFSET = 0xffff_0000_0000_0000` | `__va` |
| Linux riscv64 (sv39) | user low half; `PAGE_OFFSET = 0xffff_ffc0_0000_0000` | `__va` |
| FreeBSD amd64 | kernel text high; a direct map (`PHYS_TO_DMAP`) in the top half, plus `pmap_map` for short-lived mappings | `PHYS_TO_DMAP` |
| NetBSD / OpenBSD amd64 | same shape: kernel high, direct map in the top half | `PHYS_TO_DMAP`-equivalents |
| Windows x64 | kernel in the top half, **no full direct map**: the PFN database describes frames and MDLs map the pages a driver actually needs; page tables reached by a self-mapped PML4 entry | `MmMapLockedPages*`, `MiGetPteAddress` |
| seL4 | the kernel has its **own** page tables; a user address space contains no kernel mapping at all | mappings the kernel holds explicitly |
| Zircon (Fuchsia) | a distinct kernel aspace (top half) beside per-process `VmAspace` objects | kernel-heap objects, reached by pointer |

The constants above are from memory of each system's headers (this tree has no Linux/BSD source
checked in — `target/linuxref/` is empty); **pin them from the actual headers before borrowing
one.** The *shape* is what matters here and it is unanimous: on a 64-bit machine, user space is the
low half, the kernel and its view of physical memory are in the top half, and a physical address
becomes a pointer only through one conversion function.

### 3.3 What we take from each

- From **MINIX**: the kernel's virtual base must be *above* every user window, and that is
  asserted, not assumed (`assert(vaddr < kern_vir_start)`); page tables are reached by a kernel
  VA, not a physical address; a bounded scratch window covers what a physmap cannot.
- From **Linux/BSD**: one `PHYS_TO_VIRT`/`VIRT_TO_PHYS` pair per arch, used by *every* phys
  deref — including the page-table walk, which is the site most likely to be forgotten.
- From **Windows**: a full physmap of all of physical memory is a convenience, not a requirement;
  if the span is what we dislike, the "map what you need" form is available later.
- From **seL4/Zircon**: the end state is a kernel that is not in the user's address space at all.
  We are not doing that now, but the layout below leaves the door open.

## 4. Decision register

| # | Decision | Rationale |
|---|---|---|
| **D1** | **One physmap base per arch, somewhere no user window reaches**, with `phys_to_virt(pa)` / `virt_to_phys(va)` as the *only* conversion. x86-64 `0xFFFF_8000_0000_0000` and riscv64 `0xFFFF_FFC0_0000_0000` (each arch's top half); aarch64 `0x8_0000_0000`, in the *low* half — `TCR_EL1` sets `EPD1`, so TTBR1 is not walked and a high base would fault on every access instead of shadowing. | §3.2. Its range is what makes the invariant a property of the layout rather than a convention: on x86/riscv the kernel stays low and only the physmap is high; on aarch64 the window is above both the 1 GiB user space and the 0..32 GiB identity map, and still inside TTBR0's 48 bits. |
| **D2** | **The physmap is installed in the boot tables and in every per-process table** (landed as P2a), supervisor-only, non-executable, covering RAM **and device MMIO** (the same 0..32 GiB span the identity map covers today, or a second sparse window for MMIO if we prefer). Using 1 GiB blocks, the span is **one PDPT page on x86-64 and no page at all on riscv64 or aarch64** — SV39's root table *is* its 1 GiB level, and aarch64's `PGD[0]` already points at the window's PUD — against today's 32 PDs. | Device BARs at `0xFD00_0000`… are dereferenced by drivers; a physmap that stops at RAM would move the bug to drivers. |
| **D3** | **The identity map shrinks to the kernel image** (`0x200000..__kernel_end`), and **the user base moves above it** (64 MiB — item 38's B). Together these *retire* the class: the image is then below the user base and the physmap is above the user top, so **no kernel mapping shares a VA with any user window** on any arch. | §1. Item 38's B moved the base for the *archive*'s sake and was reverted with the move unexplained; this is the same move, made once, with the physmap present and the assertion that pins it. |
| **D4** | **Page tables are reached through the physmap** (`PHYS_TO_VIRT(table_pa) as *mut u64`), and the walk/map helpers take the same conversion. This is MINIX's `p_cr3_v`, expressed with the physmap instead of a per-object VA. | §3.1. It is also the site that would go wrong *silently*: a table page allocated inside a user window poisons every walk in the system. |
| **D5** | **Device mappings get a window, not their physical address.** `do_map_phys` picks a VA (a device window, per-process, from a bounded range) instead of `vaddr = phys`. | A BAR mapped at `VA == PA` can collide with the stack window (`0xFD00_0000` is 253 MiB, the stack is 254 MiB) and is the same shadowing hazard for a *user* mapping. Phase 3 needs this anyway. |
| **D6** | **VA-based cross-address-space copies stay the rule** (`delivermsg`, `read_from_proc`, `copy_from_user`), and VM's scratch window stays for what a physmap cannot do — reaching a frame it has no mapping for, or pinning one. | The physmap does not remove the need for "run on the owner's CR3"; it removes the need for "deref a frame as a pointer". |
| **D7** | **No separate kernel address space yet** (seL4/Zircon/KPTI shape). | It is the strongest form and the largest change: it needs a trampoline, a CR3 switch on every entry/exit, and it would invalidate the "the kernel runs on the caller's tables" assumption that ~everything currently relies on. D1–D4 leave it available later; `MAX_USER_ADDRESS` becomes the boundary to enforce when we get there. |

## 5. Staged plan

Each stage has a gate that runs without the next existing.

| Stage | Deliverable | Gate |
|---|---|---|
| **P1** | **Landed (2026-09-29).** `physmap_base` / `physmap_size` / `phys_to_virt` / `virt_to_phys` / `physmap_covers` per arch, the base pinned by module-level `const _: () = assert!(...)` (above `MAX_USER_ADDRESS`, 1 GiB-aligned, no wrap — plus `>= identity_map_top()` and inside 48-bit TTBR0 on aarch64, and inside SV39's top half on riscv64), and `hal::install_physmap(cr3)`, which installs the window in 1 GiB blocks. | Host tests in each arch crate, and a boot-test line on all three arches — `OK physmap: frame 0x... at 0x...`, written through the physmap and read back through the identity map, then the reverse. Green: `cargo test -p arch-{x86_64,riscv64,aarch64}`, `just check`, `just test-arches`. |
| **P2a** | **Landed (2026-09-29).** The window goes into the boot tables before the first per-process table is built, and **every** root constructor installs it: `boot_create_restricted_page_table` (the tables the boot servers keep running on) and each arch's `exec_create_root`. x86-64 and RISC-V also inherit it through the boot root's upper half; AArch64's private PUD is built rather than copied, so the explicit install is the only thing that puts it there. This had to come before P2b, not with P4: `walk`/`map_page` cannot deref `PHYS_TO_VIRT(table_pa)` while the tables the kernel is running on lack the window. | A new boot-test line walks the window in *every* boot process's address space and then reads one frame back from each process's own CR3 — `OK physmap reachable from every boot process's table` — on all three arches. Green: `just check`, `just test-arches`. |
| **P2b** | **Landed (2026-09-29).** `walk` / `map_page` / `unmap_page` / `clear_page` and `exec.rs` reach every table through `phys_to_virt`, via one `table_ptr` helper; `PageWalkResult::pte_virt` is now the physmap address while `pte_phys` stays the physical one. Two prerequisites the plan did not name came with it: the window has to be installed **before the first walk** — `install_physmap_in_boot_tables` now runs in each arch's entry, ahead of the in-kernel suite *and* of `load_and_prepare_all`, because riscv64/aarch64 run `tests::run_all()` first — and `arch-sim`/`arch-wasm32` needed the conversion too, since `pagetable.rs` compiles against every HAL (identity there: one address space, no MMU). | Green: `cargo test -p kernel` (423 host tests), `just check`, `just test-arches` (all six QEMU gates), `just test-dynlink-x86`, `just test-cdyn-x86`. The deliberate shadow check its gate also asked for is P2c. |
| **P2c** | **Landed (2026-09-29).** The deliberate check P2b's gate asked for: `test_walk_through_the_physmap` builds a chain whose *root* is a frame, shadows that frame's identity address on the boot tables with a zeroed decoy, and walks. It also reads the root's own chain entry back **through the identity address** and requires it to be the decoy's zero — the control that keeps the check from passing vacuously, since a walk that never consulted the physmap would satisfy the main assertion whenever the shadow had failed to take. On SV39 the root *is* the 1 GiB level, so nothing is chained; the loop covers the 4-level arches. | Green on all three arches: each boot log carries `OK physmap walk: a table shadowed by the identity map still resolves`, and none reports `the identity shadow did not take`. `just test-arches`, `just check`. |
| **P3** | The remaining derefs: `vm.rs`'s fault fill, `grants.rs`'s `s_grant_pa` read, the boot path's writes. | All gates; the grant path exercised by the existing `safe*` tests. |
| **P4** | The identity map shrinks to the kernel image, and the user base moves above it (item 38's B, made once, here). | All gates. A new boot-test assertion: **no present kernel mapping's VA intersects the user window** (and no user-accessible mapping lies in a kernel VA range). And the same **re-check dynlink** as P2b, because this is the stage that moves the base. |
| **P5** | The device window for `do_map_phys` (BARs and, later, dmabuf). | `image-x86`'s `fb: backend bochs-display` needs the BAR path, so this gate already exists; `test-drmmap-x86` covers the render node. |
| **P6** | The invariant as a standing gate: the P4 assertion plus a `grep`-able rule that no `pte_to_phys(..)` is dereferenced without `PHYS_TO_VIRT`. | The assertion runs on every boot-test arch. |

**P2b and P4 are the two that must be done carefully.** P1, P2a and P3 are mechanical or additive; P5 is Phase
3's first step by another name.

**What P2b cost that the plan did not see.** `pagetable.rs` compiles against five HALs, so the
conversion it now calls had to exist on `arch-sim` (and through it `arch-wasm32`) as well as the
three hardware arches, where it is the identity rather than a missing feature. And the two host
fixtures that fabricate page tables in ordinary buffers (`grants`, `ipc`) needed their reader fixed:
`copy_from_user` reads the caller's *virtual* address, so a test whose caller VA is a real buffer
cannot index its fake chain as if that VA were small. The `cfg(test)` identity in `table_ptr` is what
keeps those fixtures working at all — the translation is the kernel's and the QEMU suite's behaviour,
not the host suite's.

**What P1 deliberately left, and P2a took up.** P1 put the window in the *boot* tables only, and
its boot test read it inside `on_kernel_tables` — enough to prove the mapping, not enough for the
kernel to rely on it, because the kernel normally runs on a process's tables. P2a is where it
became a property of every address space. Also still open is one assertion the plan first put in
P1: "the kernel image is below the user base" **cannot be stated yet**, because on x86-64 and
riscv64 it is not true — the image ends at ~33 MiB and ~2.05 GiB against user bases of 16 MiB,
which is item 38 itself. It belongs with D3 in P4.

## 6. What this is worth

- **Retires item 38's class.** Not "the measured instance is fixed" but "a kernel mapping cannot
  share a VA with a user window", which is checkable on every boot.
- **Removes a latent time bomb.** Page tables are allocated around `0xFC00000` (252 MiB) with free
  RAM to 256 MiB, and *every* process's stack window is 254..255 MiB: a table page allocated there
  poisons every walk in the system, silently, and the allocator is one MiB of growth away from it.
  P2b/P4 make that impossible rather than unlikely.
- **Smaller address spaces.** 0..32 GiB with 1 GiB pages is 2 page-table pages per process instead
  of 32.
- **Phase 3's prerequisite.** dmabuf/GBM, the render node's mmap and the device window all want a
  frame that is *not* at its own physical address.
- **One more investigation unblocked.** The dynamic-exec hang of item 38 ends in "a fault that
  should be taken is not"; the tables it must fault through are the same tables this work is about.

## 7. Risks and traps

- **The boot path reaches physical memory before a physmap exists.** `pre_init` writes through the
  *identity* map with paging freshly enabled, and the trampoline needs the low kernel mapping.
  Both stay; the physmap is installed in the boot tables as early as the tables exist, and the
  identity map for the kernel image is never removed from the boot table.
- **Device MMIO above RAM.** A physmap that stops at RAM moves this bug into drivers
  (`0xFD00_0000` BARs, the LAPIC at `0xFEE0_0000`). D2 covers the 0..32 GiB span; if we shrink it,
  MMIO needs its own window.
- **aarch64's low 1 GiB.** Settled by the base P1 landed: the window is at `0x8_0000_0000`, above
  the identity map's top (`PUD[1..32]`), so it is neither the user window (`PUD[0]`) nor anything
  `create_low_gb_pmd_table` builds. `physmap_base() >= identity_map_top()` is the guard. A *high*
  base, as D1 first proposed for this arch, is not available while `EPD1` disables TTBR1.
- **Attributes must match where a frame is mapped twice.** The kernel image is at `VA == PA` and
  (if we choose) also in the physmap. Mixing cacheability breaks aarch64 first, and RISC-V's
  `PBMT` bits are easy to get wrong; keep the physmap normal-writeback, supervisor, no-exec.
- **The physmap must not become "the identity map with a prefix".** Anything that *needs*
  identity (the trampoline, early boot, `kmain @ 0x200000`, the boot test's archive reads) keeps
  it; the point of D3 is that the identity map is a *small, named* exception.
- **`.rules`-shaped trap:** `pte_to_phys(..)` returns a number that *looks* like a pointer and
  was one for this port's whole history. Converting it is now the rule for page tables (P2b) and P6's
  grep is what keeps it; frames, grant tables and BARs follow in P3.
- **A walk needs the window already installed.** `walk`/`map_page`/`clear_page` now fault before
  `install_physmap_in_boot_tables` has run. That is why the call sits in each arch's entry rather
  than in `load_and_prepare_all`: the riscv64/aarch64 test binaries run the in-kernel suite — which
  walks — before that, and the x86 test binary runs its own ahead of it too.

## 8. Suggested `.rules` additions

- A kernel read or write of a physical frame goes through `PHYS_TO_VIRT`; dereferencing the result
  of `pte_to_phys` directly is item 38's bug (`KNOWN_ISSUES.md`) — it reads whatever the *running*
  process has mapped at that address, which for a large server is its own image.
- A kernel-only mapping's virtual address must be outside every user window. The per-arch
  `PHYS_TO_VIRT` base is asserted against `MAX_USER_ADDRESS` at compile time; do not add a kernel
  mapping below it.
- The 0..32 GiB identity map is supervisor-only and shrinking to the kernel image; it is not a way
  to reach physical memory (that is the physmap), and no user mapping may be placed where it maps.
