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

The root's other C files are **not in the tree**. They used to print constants (`"0"`,
`"0 0 0 0 0"`), and a constant a reader cannot tell from data is worse than an absent file:

| C file | What it needs |
|---|---|
| `kinfo` | `sys_getkinfo`'s `nr_procs`/`nr_tasks`; this port's `KInfo` is `{boottime, loadinfo, reserved[14]}` with neither field |
| `loadavg` | `GET_LOADINFO` is a defined constant with no arm in `do_getinfo_handler` |
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
