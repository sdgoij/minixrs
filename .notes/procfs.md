# procfs on minixrs

`crates/fs/src/procfs` is a port of MINIX 3.3.0's `minix/fs/procfs`. The tree, the VTreeFS hooks
and the output buffer are the C design; the differences are in where the data comes from and in
one file, and both are deliberate rather than permanent.

## What it serves today

| File | Source |
|---|---|
| `/proc/hz` | `SYS_TIMES` (`do_times_handler`), ticks per second |
| `/proc/uptime` | `SYS_TIMES`, monotonic ticks split as C's `100 * ticks / hz` |
| `/proc/<pid>/status` | PM's `SI_PROC_TAB` record: name, state, pid, ppid, uid |

## How it is mounted

The server is boot process 21 (`PROC_FS_PROC_NR`). The kernel loads `/sbin/procfs` from the
initramfs with the other boot servers, so it has an `MProc`/`Fproc` slot before anything talks to
it, and VFS mounts it at `/proc` in `sef_cb_init_fresh`. The mount is resolved in MFS like
devman's: `req_readsuper` on the server, then `req_lookup` for `/proc` on MFS, then a vnode for
the server's root so path traversal can cross at the mount boundary.

The order matters in one direction: `/proc` is mounted **before** `/devices`. Boot-test check D
reads the last message VFS sent before `SYS_BOOT_COMPLETE` and expects devman's `/devices`
lookup, so a mount added after devman moves that check with it. Nothing else depends on the
order: both servers are enqueued before the scheduler starts, so VFS's `SENDREC` waits for
either.

The image's `/proc` directory is created last in `build_minixfs`, after the symlink, so the
inode numbers the layout tests and the boot test pin do not move.

The wasm system gets an instance too. `crates/wasm-servers` exports
`minix_server_procfs` over `fs::procfs::main::procfs_main`, and both front ends spawn it at
slot 21 before VFS (`tools/wasm-servers/boot.cjs`, `tools/wasm-browser/host.js`). That is not
optional: `BOOT_IMAGE` names procfs on every target and VFS's init mounts it unconditionally,
so without an instance the mount waits on a peer that never answers and VFS never reaches
`mount_devman`. The harness's own DS test client moved from slot 21 to 24, because 21 is now a
boot process.

## The gate

`just test-procfs <arch>` types `/bin/procfsprobe` into the shell on all three arches. The probe
reads the two root files and parses them, stats `/proc`, enumerates `/proc` with `getdents` the
way `ls` does, and asks for `/proc/no-such-file`. It exists because nothing else in the tree
opens a path under a user-mounted VTreeFS: the unit tests in `crates/libs/src/vtreefs` and
`crates/fs/src/procfs` call `handle_fs_message` directly, so they pass whether or not VFS ever
crosses the mount point. The probe's own parse helpers (`parse_decimal`, `is_uptime`, and the
listing check) have host tests; the end-to-end case is what proves the two sides meet.

The `list` field is the root listing. It found a real defect: the port's `fs_getdents` never
enumerated a directory's *indexed* children, and the PID directories are exactly that (their
inode index is the process slot). Worse, the positions it then handed to the ordinary-child walk
were negative for the first `indexed` slots, so `ls /proc` printed `hz` and then `uptime` once
per process slot, and no PID directory at all. The enumeration is now C's (`read.c:115`): an
indexed range walked by index, skipping an index with no inode, then the ordinary children.
`construct_pid_entries` had the matching defect and now adds a PID's files as indexed children
once, so a second listing does not repeat `status`.

The root's other C files are **not in the tree**. They used to print constants (`"0"`,
`"0 0 0 0 0"`), and a constant a reader cannot tell from data is worse than an absent file.
(`loadavg` left this list on 2026-11-21: `do_getinfo_handler` answers `GET_LOADINFO` from
`glo::LOADINFO`, which `clock::load_update` had been maintaining all along, and
`procfs_getloadavg` now does C's three-window arithmetic.)

| C file | What it needs |
|---|---|
| `kinfo` | `sys_getkinfo`'s `nr_procs`/`nr_tasks`; this port's `KInfo` is `{boottime, loadinfo, reserved[14]}` with neither field |
| `meminfo` | `vm_info_stats` (`VMIW_STATS`); VM owns the numbers and no request lets a server ask |
| `dmap` | `getsysinfo(VFS, SI_DMAP_TAB)`, which exists in `vfs/misc.rs`: only a call |
| `cpuinfo` | `GET_CPUINFO` has no arm, and `cpuinfo.rs`'s `print_cpu` prints "GenuineIntel" for vendor 0 |
| `<pid>/cmdline`, `<pid>/environ` | a copy out of the target's stack frame (C's `get_frame`) |
| `<pid>/map` | VM's regions (`vm_info_region`) |
| `<pid>/psinfo` | a binary struct joined from the kernel, PM, VFS and VM tables |

## Deviation: `SI_PROC_TAB` carries a record, not `mproc`

C copies PM's raw `mproc` array and procfs parses it against `mproc.h`. This port hands out
`arch_common::types::ProcTabEntry` instead, because `MProc` lives in the `servers` crate, a
filesystem server cannot depend on that crate, and a mirrored copy of a several-hundred-byte
struct with signal sets and timers would drift from the original without anything failing to
build. The record holds exactly the fields `/proc` prints, so adding or renaming one breaks both
sides at compile time; the layout has a host test.

The request itself is C's: `PM_GETSYSINFO` with `SI_PROC_TAB`, root-only, and the size must be
exactly one table.

## Deviation: `/proc/<pid>/status` is text

C has no `status` file; it has the four per-pid files above, all of which need a source this port
does not have. A text summary of the record PM already gives us is truthful today, and it is not
C's `psinfo` format with invented numbers in it. `psinfo` (and the rest) can be added file by
file as their sources land; nothing here prevents that.

The `State` letter is PM's view alone: `Z` for a zombie, `T` for a stopped process, `R` for
anything else PM has in use. C derives a richer set from PM's flags *and* the kernel's run-state
flags, which procfs does not have yet.

## The callback-data encoding

A vtreefs inode has one `usize` of callback data, so a procfs file's handler and the kind of
handler share it: 0 is the sentinel, a `Static(fn())` stores the pointer whole, and a
`Dynamic(fn(i32))` stores `pointer | CB_DYNAMIC`.

`CB_DYNAMIC` is the **top** bit (1 << 31 on wasm32, 1 << 63 elsewhere), not bit 0. Bit 0 was the
original choice, on the argument that a function pointer is aligned. That holds on x86_64,
riscv64 and aarch64 and fails on wasm32, where a function pointer is an index into the
indirect-function table and `root_hz` can sit at an odd index. The tag then cleared the index to
null or aliased it onto a neighbouring function, and `cat /proc/hz` in the browser system died
with `null function or function signature mismatch`. `KNOWN_ISSUES.md` item 67 has the trace.
The top bit is free everywhere this port runs: userland code is well inside the low canonical
range on the hardware arches, and a wasm table index is small.

A host test cannot catch a regression here, because the host's pointers are aligned; the gate is
`/bin/cat /proc/hz > /dev/null && /bin/echo procfs-ok` in `tools/smoke/scenario.tsv`, which the
three image boots and `tools/wasm-browser/run.js` all type at a live shell.
