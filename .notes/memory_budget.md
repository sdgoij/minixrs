# Guest memory budget, and why the coreutils gate used two boots

The coreutils gate drove two boots per arch for a while. The reason was memory, not
an exec-count limit. This note records the measurement that found it, and what
frees memory and what does not.

## The measurement

`hangdump` prints `free=<pages>` from `phys_free_pages()`. On x86_64 with `-m 256M`:

| point | free |
|---|---|
| boot (first command) | 6084 pages (24.9 MB) |
| after the 86-step core fixture | ~1965 pages (7.8 MB) |
| after 90 steps of the merged fixture | 788 pages (3.2 MB) |

The boot banner says `255 MiB detected (231 MiB usable)`, so ~206 MB is gone by the
time the shell starts. The per-process `loaded phys` lines show where: the first 16
boot processes are placed at `0x1848000..0x3315000` (24..53 MB), then `input` at
`0xb742000` (192 MB), a 139 MB jump between `fb` and `input`.

That jump is `fb`'s image. `llvm-nm -S --size-sort target/x86_64-pc-minix/release/fb`
shows `GEM_ARENA` at `0x08000000` (128 MB). The loader allocates `calc_elf_bounds`'
whole `memsz` contiguously per process, so a 128 MB `.bss` costs 128 MB of RAM at
boot even though nothing has touched it. aarch64's `fb` has no `GEM_ARENA` (it is
`#[cfg(target_arch = "x86_64")]`), so its boot leaves ~127 MB free at 256M.

Consequences. A single exec is cheap: 50 `coreutils true` in a row cost ~1 page each.
The pipeline `echo HI | cat` fails at the point above with `echo: Unknown error 32`
(EPIPE) because the second child cannot get its page(s); the shell then prints a
prompt. It is not the filp table (12 of 1024 used), not the process table (24 live
processes, unchanged), and not a leaked process. The fixture's first pipeline is
simply the first operation that needs two processes and new frames at once.

With `-m 512M` the merged 134-step fixture passes on x86_64, riscv64 and aarch64, so
the gate now runs one boot at 512M.

## What frees memory

- A process's address space: `vm_destroy` calls `free_address_space`, which walks the
  page tables and frees the frames, skipping only `VR_DIRECT` (device) regions. The
  `vm_unshared` path frees table pages and nothing else, by design, when the sharing
  of the address space was never recorded.
- The VM file cache: `CACHE_CAPACITY` is 4096 pages (16 MiB), evicted LRU with
  `pb::pb_unref` on the victim, and a dirty entry is never evicted.
- `pb::pb_unref` frees a frame when the last mapping and the cache entry both go.

## What does not

- `GEM_ARENA`: a static in the server's image. It is allocated at load and never
  freed. That is deliberate (the render node's objects need physically contiguous
  guest memory), but it fixes the x86 guest's free pool at about 25 MB at 256M before
  any userland runs.

## The per-exec page (fixed)

Before this was fixed, 50 `coreutils true` in a row cost one page per exec (10 pages
per 10, measured with `hangdump`). The source was the FPU save area:
`fpu::save` (and the aarch64/riscv area helpers) allocates one physical page the first
use of a slot, and nothing freed it. `Proc::default` zeroes `p_seg.fpu_state` when the
slot is handed out again, so the page was unreachable, and `fork_inherit` gives every
fork a fresh copy, which is why it was one page per command.

`fpu::release` now frees that page and clears the pointer. It is called on every slot
retirement: `sys_exit_handler` and `clear_endpoint` for the main slot, and
`thread::exit`/`thread::sweep_group` for workers. Re-measured: 50 execs leave
`free` at 5094 pages, unchanged across all five dumps (0 pages per 10 execs).

## Reproduce

`sh tools/smoke/feed.sh <log> <seconds> <qemu line>` with
`FEED_SCENARIO=tools/smoke/coreutils-minix.tsv`, and `hangdump` inserted every ten
steps, produced the table above. The boot free is the first `hangdump` in a fresh
boot. To see the `fb` image size, run `llvm-nm -S --size-sort` on the arch's `fb`
binary and look for `GEM_ARENA`.
