# gates.md: why each QEMU gate exists

The Justfile no longer has a recipe per gate; `cargo xtask gate <name>`
builds (`just build <arch>`, `just system-image <arch>`, or a
`prepare-<name>` recipe) and then boots the guest. The gate table is
`crates/xtask/src/gate.rs`, and the scenarios are `tools/smoke/*.tsv`.
These are the comments the recipes carried. A heading is the old recipe name;
the gate drops the `test-`/`image-` prefix and moves the arch to `--arch`
(`test-fork-x86` is `cargo xtask gate fork --arch x86`).

## test-system-image-x86

# Boot the split pair and drive the userspace smoke scenario into its shell, so a
# release only ships a system image that was actually booted (the self-contained
# `image-<arch>` is booted the same way). The dependency builds both images first.

## test-fbmmap

# The `/dev/fb` `mmap` path: a userland `mmap` of the framebuffer must alias the device's
# own memory, which every present path (`wserver`, `/sbin/wlserver`, and the clients that
# read the frame back) depends on. It broke silently once: after `VM_MAP_PHYS` began
# returning a *window* rather than the physical address (`.notes/physmap.md` D5), the fb
# server reported that window as the physical base, so every client's `mmap` landed where
# the device is not and read a black frame. `/bin/fbmmap` does the whole check itself —
# paint, then read back the block and the boot pattern — so this needs no screenshot and
# no GL host, and it runs on every arch.

## test-interactive-bash

# Interactive bash: readline polls the console instead of blocking on a line read,
# so this drives a *typed* line at `bash-5.3#` — the part the `bash -c` scenario in
# `test-bash-<arch>` cannot see. It boots the split pair (the shipping artifact),
# which carries bash when its `target/bash/<arch>/bash` exists; `build-bash` first
# so the image the gate builds has it.

## image

# One self-contained, bootable artifact per arch: the kernel with the initramfs
# and root filesystem embedded, so QEMU needs no separate disk. With no virtio
# disk attached, MFS mounts the embedded image through the ramdisk driver
# (`fs::block_io::bdev_driver_root` falls back to it when the preferred driver
# has no device).
#
# The artifact is an ELF, which is what `-kernel` loads; x86 carries the kernel
# as a segment of its multiboot trampoline ELF, the other arches are the kernel
# itself. A raw disk image for real hardware would instead be the (unwired)
# `tools/mbr.S` + `tools/stage2.S` route (not wired up).
#
#  Each recipe boots what it produced and drives the userspace smoke scenario into its
# shell, so a bad artifact fails here rather than in someone's hands. The boot timeout
# bounds the guest - 15 s, against a guest that reaches the shell in about two, with the
# scenario's own budget set a second inside it - so a guest that stops making progress
# fails the recipe instead of only being slow. It is a parameter so that the release job
# (`.github/workflows/image-release.yml`, called by `ci.yml`) can pass a larger one: a CI
# runner emulates the guest, and that is a property of the runner, not of the artifact.
#
# The QEMU command mirrors the matching `run-*` recipe minus the disk - which is
# the point - so the display and input devices are passed too: without virtio-gpu
# `fb` fails to initialise on riscv64/aarch64 and wserver has no framebuffer (x86
# would still look fine, because QEMU keeps a default VGA adapter under
# `-nographic`).
#
# `/usr/bin/timeout` is spelled out because on Windows a bare `timeout` resolves
# to the Windows TIMEOUT.EXE instead: a recipe shell inherits System32 ahead of
# the MSYS bin dir, and that binary's command line is unrelated (the same trap
# applies to `find`, `sort` and `more`).
# Build the single-file boot artifact for an arch. `boot-timeout` is what the
# self-check gets (see the note above).

## image-x86

# One self-contained, bootable ELF per arch, booted and driven through the userspace smoke scenario
# (`tools/smoke/scenario.tsv`): a program exec'd from the image, a file written, that file read back
# — the same steps `run.js` drives in the browser. The guest is not left to print markers on its own;
# the scenario is typed into the shell that a user gets, which is what makes this a check on the
# artifact rather than on the servers having started. `wserver: ready` stays an assertion as well:
# it caught a real regression (a browser-ready desktop, dead on the arches) that the scenario could
# not see — the shell worked while the window server was stuck, finding 66.

## test-gpu3d-nogl-x86

# The other half of 3a's degrade path, and the one `image-x86` cannot cover: a
# virtio-gpu device the host offers but did *not* enable GL on. Such a device answers
# no VIRTIO_GPU_F_VIRGL, and the probe has to say so and stop there - the capset and
# context commands it would otherwise send are exactly the ones a feature-less device
# answers with errors (or, worse, does not answer at all). A plain `virtio-gpu-pci` is
# that device on every x86 host, GL-capable or not, and being 2D-only it needs no GL
# display backend: `-nographic` is enough, so this one boot carries both halves of 3b-2:
# the fb server reports the device as no-3D, and then a client opens the render node and
# is told so itself. The step is the client rather than the boot smoke because the client
# is the stronger proof that the guest got here — it execs a binary from the image, opens
# a device, and takes a path through VFS and a driver — and because the smoke proper is
# `image-x86`'s. `tools/smoke/drminfo.tsv` has the rest of the reason this gate uses a
# device without GL.

## test-drmmap-x86

# The node's *memory objects*, which is 3b-3 and a different claim from 3b-2's: a client
# creates one, maps it, and proves the mapping is that object's memory. Same device as the
# gate above and for the same reasons (it types, so it needs the 2D-only device; and
# `RESOURCE_CREATE` on a device without GL is the `RESOURCE_CREATE_2D` +
# `ATTACH_BACKING` path, which a later client meets whenever the host has no GL).

## test-drmprime-x86

# The render node's PRIME handoff (`tools/smoke/drmprime.tsv`): `diag drmprime` exports a
# memory object to a dma-buf descriptor, checks the descriptor's own kind and device with
# `fstat`, imports it back to the object's handle, and reads the bytes through it. The device
# is the same plain `virtio-gpu-pci` the other node gates use, for their reason: sharing is a
# property of the node's object table, not of GL, so the degraded device exercises the same
# code without a GL host to drop serial bytes.

## test-readlink-x86

# `readlink(2)`, which nothing proved until now: the three pieces that move a filesystem
# server's bytes into a user process (`tools/smoke/readlink.tsv` has the whole reason) are all
# stubs of some kind before this, so the gate is the first exercise of the path. No device is
# needed beyond the ones a plain boot has, and the step's own expectation is the assertion.

## test-symlink-x86

# `symlink(2)`, the write half of the readlink gate: a link MFS creates at run time, read
# back out through the same path (`tools/smoke/symlink.tsv` has the whole reason). Nothing
# had ever created one, so this is the first exercise of it, and the step's own expectation
# is the assertion. No device is needed beyond the ones a plain boot has.

## test-link-x86

# `link(2)` and `rename(2)`, the acceptance test for KNOWN_ISSUES.md item 37 - green since that
# item's faults were fixed (2026-10-02). It was *red* on purpose until then, and written to the
# behaviour the calls should have rather than the behaviour they had, so no step here was edited
# when the fix landed: the same file that measured the fault is what now passes.
#
# A step's fields are separate claims - each call's own errno, the state it left, and a
# neighbour file's bytes - so a call that ran against the wrong name cannot read as one that
# worked. x86 only: the fault was in VFS/MFS, which the other arches share.

## test-drmgl-x86

# The render node's client on the GL device, which is the one gate that types into a guest with a
# GL display backend: host GL drops serial bytes mid-line, so this recipe sets `FEED_PACE` and
# the smoke driver writes the step a byte at a time and refuses to believe it until the guest has echoed
# the whole line (`tools/smoke/drmgl.tsv` has the measurements and the rest of the reason).
#
# `blob=on` because the blob feature is only available on this host's GL device, and the two
# assertions below are the two ends of it: the fb server's boot line says the device offered it,
# and the client's line says the node reports it.

## test-coreutils-wedge

# Acceptance test for KNOWN_ISSUES.md 12 — `coreutils seq 3` must write its three lines and
# the next tool must read them back. It is the gate that made the multicall wedge findable: a
# child used to die inside `clap` on a corrupted pointer (item 12's open half), and these steps
# are exactly what that defect broke.
#
# Green as of 2026-09-22: the last cause was the kernel clobbering a live process's XMM
# registers on entry (`crates/kernel/src/fpu.rs`), and the recipe went green with no edit to
# these steps, which is what it was written to do. Kept separate from `image-x86`'s boot smoke
# so that gate keeps measuring what it means to (that the image boots), and out of
# `test-arches`, as it was while red, so a failure here is never reported as a boot or arch
# failure.

## test-coreutils-wedge-aarch64

# The aarch64 half of the coreutils gate: the multicall is embedded on aarch64 now, and
# `seq 200 > file` is the sequence that used to write an empty file when a context switch
# dropped the caller's SIMD registers (KNOWN_ISSUES aarch64 #9).

## test-coreutils-minix

# The applets that were excluded from `feat_minix` for platform reasons, on every arch:
# `touch` is the filetime path (`utimes`/`lutimes` and `stat`'s times), `env` the exec
# and signal path (`execvp`, `sigprocmask`, `signal`), `rm`/`chmod` the `safe_traversal`
# path over the C library's `openat` family, `chgrp`/`chown` the `wrap_chown` path over
# VFS_CHOWN and `lchown`'s `AT_SYMLINK_NOFOLLOW`, `groups` the `getgrouplist` path,
# `id` the passwd/group name lookups plus `getlogin`, `logname`/`whoami` the
# same lookups from one id each, `install` the copy/mode/owner path,
# `mkfifo` the VFS_MKNOD path, and `chroot` the VFS_CHROOT path.
# `tools/smoke/coreutils-minix.tsv` says which step covers which.
#
# One boot, at 512 MiB. The fixture is the whole set (134 steps), and 512 MiB is
# what it needs: x86's fb server keeps a 128 MiB `GEM_ARENA` static
# (`crates/servers/src/fb.rs`) that the boot loader allocates with the image, so a
# 256 MiB guest has only ~25 MiB free and the sequence exhausts it (the first
# pipeline fails with EPIPE). The arena is deliberate, for the render node; the
# gate gives the guest room for both.
#
# Every arch paces its input (`FEED_PACE`): the console drops bytes mid-line
# under a burst, which runs a truncated command. Pacing costs a `sleep` per
# group under MSYS, so the budgets cover a paced scenario.

## test-fileread

# The read path over a file written this boot, per arch: `tools/smoke/fileread.tsv` writes
# `seq 1 10000` (twelve blocks, so blocks 8-11 sit behind the single-indirect block) and hashes
# what reads of a chosen size return through `/bin/filereadprobe`.
#
# The defect this pins is `KNOWN_ISSUES.md` item 60: an indirect entry `write_map` has not filled
# yet is a zero, and reading that zero as `0xFFFFFFFF` made it a valid block number, so those
# blocks all resolved to block 0. It is per arch because the read crosses VFS's grant, MFS's block
# loop and the kernel's copy, and each arch builds its own tables; but the defect itself was
# cross-arch, and x86 is the arch it was measured on.

## test-mmapanon-x86

# The shared anonymous mapping gate: VM allocates the frames at map time, so a
# store the child makes after the fork is visible in the parent's view.

## test-writeback

# The MAP_SHARED *file* gate: the store's visibility to other mappers is one half and
# was already there; the other is that the file receives it when the mapping goes away
# (KNOWN_ISSUES.md item 48). `tools/smoke/writeback.tsv` creates its own file and
# reopens it with a new fd, which cannot see the cache, so only a real writeback can
# satisfy the expectation. Per arch because the writeback goes through VM's own
# self-mapping (`vm_mappage`) and VFS's grant, both of which are arch-specific.
#
# Input is paced (`FEED_PACE`) because the first step is a whole command line in one
# write and a guest that drops the tail of a burst loses it: riscv64 failed 1 run in 3
# on the unmodified tree with `# /bin/echo writeback-marker > wb.t` in the log, which
# the paced path turns into a verified echo and a retry instead of a lost step.

## test-long-path

# Long paths must resolve. KNOWN_ISSUES.md item 19: a pathname of 24 bytes or more
# killed VFS — it wrote the REQ_LOOKUP terminator one byte past the 56-byte message
# at exactly 24, and truncated anything longer to 24 and resolved the wrong name.
# The path now travels in a grant. Steps bracket that boundary so a regression names
# the length it stopped at, and each read-back is a marker no other step can print.
# *Creating* a name past 28 bytes is item 20, still open, and is not what this gate
# measures. x86 only, like `test-coreutils-wedge`; the other arches have images and
# could run the same scenario.

## test-burst

# A 240-byte command line. The tty has four console lines and every one used to drain the one
# kernel serial ring, so a burst was split between them; the lines that are not the active console
# keep the default termios (canonical, ECHO on) and printed what they took, which the console showed
# as the argument's tail ahead of the command's own echo (the "rotated" burst in KNOWN_ISSUES).
# Each step's argument starts with a tag its output must begin with, so a byte lost to QEMU's stdio
# transport still passes while a line the shell never ran fails: the exact argument is deliberately
# not the expectation, because that loss is a separate, known limitation and this gate is about the
# line reaching the shell.
#
# Paced (`FEED_PACE`), for the same reason the read-cost gate is. A 240-byte write reaches QEMU in
# one go and its 16-byte receive FIFO overflows before the guest drains it, losing the middle of the
# line and the newline with it (measured 2026-11-16: the gate failed about one run in two on x86_64
# and riscv64, the echo stopping mid-line, with the same tree that passes the rest of the time).
# That is `cargo xtask smoke`'s transport limitation, not the line discipline: `type_step` writes
# a byte at a time and refuses to go on unless the whole command came back, so what this gate
# measures stays the guest's behaviour.
#
# The dispatcher passes no timeout: each arch recipe's own default is the budget it was measured
# with. An earlier version forwarded a flat 30 s, which silently overrode riscv64's and aarch64's
# 60 s and made the gate fail on a slow boot rather than on a dropped line.

## test-ttytimeout

# The tty read timer (MIN/TIME). `ttytimeout` switches the console to raw mode with VMIN=0 and
# VTIME=2, reads without input, and expects zero bytes back once the deadline passes. Nothing
# else in the tree observes a timer the tty server owns: the deadline is a number checked on
# the console pump's own wake (C `tty.c` fires it from `expire_timers`), so a regression shows
# up here as a read that never returns rather than as a wrong byte.

## test-pipeblock

# Blocking pipe reads and writes (KNOWN_ISSUES.md 32): `tools/smoke/pipeblock.tsv`. A read of
# an empty pipe waits for its producer instead of answering EAGAIN, and a write bigger than the
# buffer waits for the reader to drain it, on every arch because the pipe is VFS's own code.

## test-threads

# Kernel threads (KNOWN_ISSUES.md 8): `tools/smoke/threads.tsv`. `threadtest` spawns four
# kernel threads that block in PM IPC, bump a shared counter and exit, and joins them;
# `forkthread` forks from a worker and requires the child to be a copy of that worker. Per
# arch because the thread frame is: the kernel records the stack top and the TLS base in
# each arch's own layout (`SYS_thread_create`/`SYS_thread_set_tls`).

## test-read-cost

# The per-command read cost of a large binary. The scenario runs two invocations of the multicall,
# each reading no file data: the first costs ~1080 virtio reads of 4 KiB because the first text
# fault pre-faults every non-executable region of the image (KNOWN_ISSUES #5), and the second
# costs ~9 because VM's file cache serves those pages from the same boot. The total was 1146 when
# the multicall gained `ls`, `cp`, `mv` and the small unix set (KNOWN_ISSUES 62), one zone-sized
# request per added page, so the ceiling is 1200; it still fails if either the cold start grows or
# the cache stops serving the warm one, and the warm half is the property that matters for a shell
# running many applets. Paced input (`FEED_PACE`) because the traced stdio console drops bytes, and
# a step that never arrived would fail before the count mattered.

## test-bash

# Boot the bash that `build-bash` produced and drive `tools/smoke/bash.tsv` at
# it: the version banner, a `-c` command line, an arithmetic expansion, a loop, a
# redirection read back through a second process, an external command (bash's
# fork+exec path) and `$PWD` from its own startup. Nothing else in the tree boots
# bash, and the shared smoke scenario cannot: it types into the minix shell and
# waits for its `#` prompt, which an interactive bash would replace with
# `bash-5.3#`.
#
# The injection is what puts bash in the image: it is deliberately not a
# `BOOT_BINS` entry, so an image build never depends on bash having been built.

## test-bash-riscv64

# The shell does not fit the 16 MiB default here either; riscv64 boots fine at 8192, the
# value `image-riscv64` uses, so this gate uses it too (`build-x86` records what the window
# costs the guest and why x86 cannot take the larger one).

## test-dynlink-x86

# Boot the dynamic-linking artifacts and drive `tools/smoke/dyn.tsv` at them.
# `/bin/dynhello` is an `ET_EXEC` with `PT_INTERP /libexec/ld.so` and `DT_NEEDED`
# for three shared objects, one of which names another three times, so the strings that
# live only in those objects are what prove the loader chose a base for each of them,
# followed a dependency of a dependency, resolved names across objects, applied
# each object's own `RELATIVE` fixups and `R_X86_64_COPY`, and ran the objects'
# initialisers in dependency order. Two further steps ask for a fork after the
# load (the child calls into both objects) and an exec after it (the replacement
# program loads again).
#
# The negative check is the centre of this gate: no `dynlink` string may be in
# `/bin/dynhello`. Were one there, the program could print it with no loader at
# all and the boot below would pass while proving nothing. (The check is against
# the built program, which is the file `/bin/dynhello` is a copy of.)
#
# The injection adds the loader's *own* test objects to a standard image: `libdyn*.so`
# and `/bin/dynhello` are deliberately not `BOOT_BINS`, so no image carries them. What
# they need in order to run — the loader, `/lib/libc.so` and `/bin/dynclib` — an image
# does carry, and this gate is where the shipped `/bin/dynclib` is exercised, so a
# regression in it is caught here rather than only in a gate that had built its own
# copy.
#
# The last five steps are `dlopen` (Phase 6). `tools/dynopen.c` asks the loader for
# `libdlopen.so` at run time — the object is in nothing's `DT_NEEDED`, which is the case a
# driver lookup is — and reaches it through the handle, and through `RTLD_DEFAULT` after an
# `RTLD_GLOBAL` load, and again after a `dlclose`. The two failing steps assert the loader's
# own message rather than just "it failed": a path that is not there, and `libtls1.so`,
# whose thread-local the loader cannot place, because the module it lays out is fixed at
# startup for every thread.

## test-dynlink-riscv64

# The dynamic-linking gate for riscv64: the same `tools/smoke/dyn.tsv` scenario as
# x86_64, which is deliberately arch-neutral — it asserts on symbols only the shared
# objects contain, on an errno read through the loader's TLS, and on the program's
# own constructor, none of which depends on the instruction set.

## test-cdyn-x86

# The gate on `just cdyn` — the recipe a user is told to use for a program of their own.
# Every other recipe here builds its programs with `cargo xtask build dynlink` or
# `cargo xtask build dynlibc`; this one goes through the recipe's own interface and boots what
# it produced, which is what makes the two commands `cargo xtask build cdyn` prints for a user
# something that has been run at least once. The link underneath is that script's, the same
# one the shipped `/bin/dynclib` gets.
#
# The injection is `MINIXFS_EXTRA` set inside the recipe, so the Justfile's
# `MSYS2_ENV_CONV_EXCL` covers it. A user setting the variable in their own shell has to
# cover it themselves, which is why the hint `cargo xtask build cdyn` prints names that variable on
# Windows.

## test-gltriangle

# The §6.10 stage 3c-2 gate: boot the split pair with /bin/gltriangle and the seven
# Mesa DSOs in the *system* image and drive `tools/smoke/gltriangle.tsv`, whose one
# step wants the client's `gltriangle: pass` line. This is the end-to-end proof of the
# C GL stack — surfaceless EGL, the gallium `softpipe` driver, GLSL and a checked
# readback — and of the loader's `DTPREL` (a general-dynamic thread-local access).
# The DSOs are ~36 MiB, so the system image carries them with `MINIXFS_BLOCKS` raised.
# Needs `just build-mesa <arch> --build` and `just libcxx <arch>` first.

## test-gltriangle-virgl-x86

# The 3c-3 gate: the *same* client as 3c-2, differing only in the device Mesa is
# handed. The guest gets a GL device (`-device virtio-gpu-gl-pci,blob=on`), so its
# render node opens and surfaceless EGL takes the `virgl` driver on it instead of
# falling back to `softpipe`. That path exists because libdrm is built as its
# OpenBSD/DragonFly branch and the render node answers `DRM_IOCTL_GET_PCIINFO`
# (`.notes/wayland.md` §5 row 24): the loader reads the virtio-gpu ids from it and
# picks `virtio_gpu`. `FEED_PACE` because a GL display backend drops serial bytes
# mid-line (see `test-drmgl-x86`); the assertion is the scenario's
# `gltriangle: pass`, which is a real readback.

## test-wlgl-x86

# The 3d-a gate: the client renders a GL surface with Mesa on the render node and
# commits it through /sbin/wlserver to /dev/fb, then reads the composited frame back.
# `GL_RENDERER=virgl` is asserted too, so the frame is the host GPU's and not the
# software fallback's. It is a dev-host gate like 3c-3 — headless CI has no GL.

## test-gui-x86

# The M1 gate: the in-house renderer draws a quad and a line of text into a wl_shm pool,
# /sbin/wlserver presents it, and the client checks /dev/fb. The guest has no GL device, so
# Mesa takes softpipe and the gate runs headless, like `test-gltriangle-x86`.

## test-gbmtest-x86

# The 3d-b-3 gate: a GBM buffer object made on the render node, exported to a dma-buf by
# `gbm_bo_get_fd`, and imported back by `drmPrimeFDToHandle` (`.notes/wayland.md` §6.10). It
# is a dev-host gate like 3c-3 and 3d-a — GBM allocates through the node's DRI driver, which
# needs a GL-capable host — so headless CI does not run it. `dri_gbm.so` is the backend
# `libgbm` `dlopen`s, at `/lib/dri_gbm.so` (`gbm-backends-path`).

## test-wlfull

# The 3d-b gate: a whole-output `wl_shm` client is presented by re-pointing the scanout at its own
# buffer, so the compositor copies nothing (§6.10). `/dev/fb` no longer holds the frame (the frame
# *is* the client's pool), so `cargo xtask probe wlfull` checks the display with a QMP screendump and the
# fb server's `fb: scanout src` line. On x86 the output device is a `virtio-gpu`, not `bochs`: the
# re-point is a virtio-gpu capability (D11); on riscv64/aarch64 the `virtio-gpu` is already the
# output, so only the full-output client is new.

## test-wldmabuf-x86

# The 3d-b-4c gate: a linux-dmabuf client's render-node object is scanned out with no copy
# (§6.10). The output is a virtio-gpu with no bochs-display, so the render node is served from
# the output device itself (D11) — the client's render resource and the scanout are one device,
# which is what lets the fb server re-point the scanout at it. A QMP screendump of the client's
# colour and the fb server's `fb: scanout src` line are the evidence.

## test-wldmabufgl-x86

# The 3d-b-4d gate: the GL client's render target *is* the dma-buf `wl_buffer`, which the
# compositor scans out of the client's own object — no compositor copy. It is a dev-host gate
# like 3c-3/3d-a/3d-b-3 — headless CI has no GL. The QEMU line puts a 2D `virtio-gpu-pci`
# first and the GL device second: the client renders on the GL node and the fb server re-points
# the 2D scanout at the client's object, so the screendump (which follows the first, 2D device)
# shows the frame — a GL device *as* the output would need a host blob this host refuses (D11).
# `-display egl-headless` is what a GL device needs. The one copy on this path is the client's,
# not the compositor's: this host's virgl renders into a host-side texture, so the client
# resolves it and transfers it into the object before committing (`.notes/wayland.md` §5, and
# `tools/egl_bo_probe.c` is the measurement).

## test-wlcompose-x86

# The 3d-b-5b gate: two GL surfaces one process rendered are composed on the render node by a
# helper in *another* process, and the peer reads the composed frame back. It is a dev-host gate
# like 3c-3/3d-a/3d-b-4d — headless CI has no GL. The peer only passes if it read the peer-drawn
# colours out of the returned frame, so this measures cross-process sampling, not merely a crash.
# The devices are the 4d gate's — a 2D `virtio-gpu-pci` and a GL device — the pair the port's role
# selection (D11) has been validated with; `-display egl-headless` is what a GL device needs.

## test-wlscene-x86

# The 3d-b-5c/5d gate: two dma-buf surfaces committed on one connection are composed on the GPU by
# the helper process and scanned out as one frame. `diag wlscene` is a plain static client; the
# helper is the dynamic `/bin/wlcompose`, injected with the Mesa DSOs. The devices and the GL
# display are the 5b gate's, plus QMP so the host can read the display the frame lands on.

## test-uds-x86

# UNIX-domain sockets: /bin/udstest round-trips a socketpair and a
# bind/listen/connect/accept connection through the /dev/uds server — Phase 0 of
# .notes/wayland.md. build-x86 embeds the binary; the scenario types the command into
# the shell and wants the whole line back.

## test-pty-x86

# The terminal path's readiness: /bin/ptytest opens a pty master, forks a slave,
# and must come back from a `poll` on the master when the slave writes. Nothing
# woke a master poller before the tty server reported it, which is why /bin/wterm
# drained the master in an EAGAIN loop instead of polling. build-x86 embeds the
# binary.

## test-select-x86

# Readiness primitives: /bin/seltest drives select(2) and poll(2) over pipes and
# checks a real timeout on each — Phase 0 of .notes/wayland.md. build-x86 embeds the
# binary; the scenario types the command into the shell and wants the whole line back.

## test-eventfd-x86

# eventfd: /bin/eventfdtest exercises the counter semantics and a cross-process
# wake of a blocked poll — Phase 0 of .notes/wayland.md. build-x86 embeds the binary.

## test-timerfd-x86

# timerfd: /bin/timerfdtest drives one-shot, periodic and absolute timers (and a
# disarm) through blocking polls — Phase 0 of .notes/wayland.md.

## test-epoll-x86

# epoll: /bin/epolltest drives a persistent interest set — add/modify/remove,
# level-triggered readiness over an eventfd and a pipe, and a blocked
# `epoll_wait` woken by a child process — Phase 0 of .notes/wayland.md.

## test-wayland-x86

# wayland: /bin/waylandtest drives the Phase 1a protocol handshake (registry and
# wl_display.sync) between a forked client and server over /dev/uds — .notes/wayland.md
# §6.11. build-x86 embeds the binary.

## test-wlshm-x86

# wlshm: `diag wlclient` drives the Phase 1b wl_shm present path — it draws into a
# memfd pool, passes the fd to /sbin/wlserver (boot proc 20), commits a surface,
# and reads the composited frame back from /dev/fb — .notes/wayland.md §6.11. build-x86
# embeds both binaries.

## test-wloverlap-x86

# wloverlap: the scene z-order gate. Two connections, two output-sized windows, and a
# recommit by the lower one; the frame must still belong to the upper one.

## test-wlkey-x86

# wlkey: `cargo xtask probe wlkey` drives the Phase 1c input path and the Phase 2a
# keymap — `diag wlkey` binds wl_seat's keyboard, resolves the `keymap` event's fd
# and checks its bytes (.notes/wayland.md §6.12), commits a surface, and a key injected
# with QMP reaches it as `wl_keyboard.key` — .notes/wayland.md §6.11. A host probe rather
# than a tsv scenario: the key has to be a real device event, and the serial console
# the tsv gates type into is not one.

## test-wlx-x86

# wlx: `diag wlx` drives the Phase 2b xdg_shell path — it maps an xdg_toplevel, the
# server sends `configure`, the client acks it and commits a frame of the configured
# size, and the frame reaches /dev/fb — .notes/wayland.md §6.12.

## test-wlfocus-x86

# wlfocus: `cargo xtask probe wlfocus` drives the Phase 2c focus path — `diag wlx2` opens
# two connections, maps a window on each, and a QMP key must reach the focused one as
# the focus moves between them — .notes/wayland.md §6.12. A host probe, like wlkey's: a key
# has to be a real device event, which the serial console is not.

## test-wlmotion-x86

# wlmotion: `cargo xtask probe wlmotion` drives the pointer-interpolation path — `diag wlmotion`
# maps a window, the host moves the absolute pointer in one event, and the client must
# receive a stream of steps ending where the device landed.

## test-wlpointer-x86

# wlpointer: `cargo xtask probe wlpointer` drives pointer-follows-focus — `diag wlpointer` maps
# a full-output window under a 64x64 one, and moving the pointer between them must move
# the focus and the keys with it.

## test-wldemo-x86

# wldemo: `cargo xtask probe wldemo` drives the widget layer — `diag wldemo` is one window with
# a text field, a list and a button, and the keys it is sent have to type, move the
# selection, fire the button and reach the client as its own state report.

## test-wlpanel-x86

# wlpanel: `cargo xtask probe wlpanel` drives the session panel, which is the session's own
# first program: `/etc/rc` starts `/bin/session`, it waits for the shell's prompt and
# starts `/etc/session`, and `diag wlpanel` maps a 200x32 layer strip at the output's
# top-right. A click inside its launcher button has to reach it and start an app. This
# is the shell milestone's gate, together with `test-rc-x86`'s chain.

## test-rc-x86

# rc: `cargo xtask probe rc` boots and types nothing. `/etc/rc` names the session manager,
# which waits for the shell's prompt, ends that line and starts `/etc/session`, so the
# chain from init to the session's first program has to happen on its own.

## test-alloc-x86

# alloc: `cargo xtask probe alloc` types `allocprobe`, which grows the rt heap across several
# chunks and frees all of it (a fully free chunk must be munmapped, so the region count returns
# to its baseline), then churns 200 x 1 KiB blocks inside one chunk (a freed block must be
# reused, so the count does not grow). A count that climbs is a leaked region or a chunk handed
# out twice. `/bin/allocprobe` prints its own verdict, so `--arch riscv64` or `--arch aarch64`
# boots the same command on the other kernels after `just build-riscv64`/`build-aarch64`.

## test-fb-x86

# fb: `cargo xtask probe fb` takes a QMP screendump and reads its corners. The fb server paints
# the boot frame as three vertical thirds, red then green then blue, so the pixels prove the
# mode set and the BAR mapping with no input and no guest-side reporting. `/bin/fbmmap paint`
# overwrites the top-left block and `restore` repaints it red for this reason: a run left
# half-restored fails here.

## test-exec-loop-x86

# exec-loop: `cargo xtask probe exec-loop` runs a command N times and samples `memstat` and
# `regions` every K execs, which is how the repeated-exec leak is measured: free pages that fall
# a little per exec, or a wedge at a round exec count, both show up in the samples. It types
# into the guest for every exec, so it never samples past an exec that has not finished, and a
# wedge is reported by sending `hangdump` rather than guessed at.

## test-wlxd-x86

# wlxd: `diag wlxd` drives the Phase 2d damage and cursor path — it damages a small
# rectangle and checks the rest of the frame survives, then gives the pointer an
# image and finds it on /dev/fb — .notes/wayland.md §6.12.

## test-wlxe-x86

# wlxe: `diag wlxe` drives the Phase 2e panel, popup and decoration path — it maps an
# opaque window, then a zwlr_layer_shell_v1 panel and an xdg_popup over it, requires
# the decoration manager to answer client-side, and checks both are composited where
# they asked and no wider — .notes/wayland.md §6.12.

## image-riscv64

# riscv64's bash is the one that does not fit the default embedded filesystem: the release
# injects it (`MINIXFS_EXTRA`) and the image is embedded in the kernel, where 16 MiB is full
# before bash goes in, so the build panics with `zone 4096 out of bounds`. The image is built
# at the system images' 32 MiB instead; the other arches' bash fits in 16 MiB. (aarch64's
# own embedded image is 32 MiB for the uutils multicall, not for bash.)

## test-qemu-riscv64

# RISC-V exits via SBI SRST (no exit-code device in this QEMU build), so the
# gate asserts the suite's completion marker in the serial log.

## test-qemu-aarch64

# AArch64 exits via PSCI SYSTEM_OFF (always exit code 0 - no exit-code device
# and no semihosting on this QEMU build), so the gate asserts the marker.

## test-procfs

# The per-arch QEMU gates a change to a VA layout, a HAL constant or anything arch-gated
# has to be followed by. They cover different things and none substitutes for another:
# `test-qemu` runs the in-kernel suite (`crates/kernel/src/tests.rs`, behind the
# `qemu-tests` feature), `test-boot` boots to a shell and runs `boot_test.rs`, and
# `test-fork` forks a process and checks both directions of the copy. The host suite
# cannot stand in for the first, because `tests.rs` is compiled only under `qemu-tests` -
# which is exactly how `syscall_brk` came to assert x86_64's heap base and fail on aarch64
# alone: `cargo test` never saw it.
#
# Grouped by arch so each arch's userland/coreutils build is reused, and stops at
# the first failing gate like the individual recipes (the failing log is on
# stdout).
#
# Every QEMU gate, on every arch, in one command.
# The procfs mount, per arch: `tools/smoke/procfs.tsv` runs `/bin/procfsprobe`, the only
# client that opens a path under a user-mounted VTreeFS. It reads `/proc/hz` and
# `/proc/uptime`, stats `/proc`, and asks for a path under it that is not there, so a mount
# that never crossed the VFS boundary reads as a failure rather than as silence.

## test-fork

# Fork isolation, per arch: `tools/smoke/fork.tsv` runs `/bin/forktest`, which forks and checks
# all three directions of the copy - the child's write must not reach the parent, the parent's
# post-fork write must not reach a child that only reads that page, and a *kernel* write into a
# page the child never touched must not reach the parent either. That last one is the answer to
# the trade item 27 records: the kernel cannot take a page fault in the middle of a copy, so it
# breaks the copy itself (`cow_prepare_write`). Per arch because each arch's `vm_paging_fork`
# builds the COW mapping itself (aarch64's AP[2:1] encoding is its own).
#
# `/bin/forktest` is a `BOOT_BINS` entry, so every image carries it, and this is the gate the
# per-arch fork entries in `KNOWN_ISSUES.md` point at.

## test-winprobe

# The user-window alias probe, per arch: `tools/smoke/winprobe.tsv` runs `/bin/winprobe`, which
# asks whether a VA the process was never given is reachable. The invariant is that it is not -
# `KNOWN_ISSUES.md` item 16 - and it is per arch because the fresh exec root is built per arch:
# aarch64 used to fill the window with EL0-RW blocks aliasing the kernel's physical allocator
# (item 16, fixed 2026-10-02), RISC-V does not copy the window's entry at all, and x86's
# identity map stops at the user base. All three answer `reachable=0` now.

## test-waitid

# `waitid` decoding, per arch: `tools/smoke/waitid.tsv` runs `/bin/waittest`, which forks a
# child that exits 42 and checks that `waitid` reports `CLD_EXITED` with `si_status == 42`, that
# `P_ALL` names the child it took, and that `WSTOPPED`/`WNOWAIT`/an empty option set are
# refused. The encoding is PM's and therefore arch-independent, but the gate runs per arch for
# the same reason `test-fork` does: it forks, and an arch whose fork path is broken should fail
# here rather than only in the gate that happens to run on it.

## test-cabi

# The C libc ABI, x86 only: `cargo xtask build c-hello` builds `/bin/cabi` for x86_64
# alone. `tools/smoke/cabi.tsv` asserts the `sockaddr_in` round trip (the only place
# the header's layout and `minix-libc`'s decoder meet), `pipe2(O_NONBLOCK)` reaching
# the pipe's read path, and an anonymous `mmap`.

## test-cdirect-x86

# The gate on the direct link: `/bin/cdirect` is `tools/hello.c` compiled by clang and
# linked by LLD against `libc.a`, with no rustc anywhere in the line. The scenario's
# line can only be produced by running it.

## test-toolchain

# The in-guest toolchain gate: the guest's own clang compiles `tools/hello.c` and
# links it with the guest's ld.lld against `libc.a`, and the program runs. The
# staged root is required (there is no build of it in CI: it costs an LLVM cross
# build), and a missing root is refused by name rather than booted as a failure.

## test-tblgen-x86

# The TableGen target, in two boots: the guest compiles every source of
# `obj.LLVMTableGenBasic` plus `llvm-min-tblgen.cpp` and links `llvm-min-tblgen`
# in the first, then a fresh boot runs the binary on a `.td` probe. 2 GiB is a
# measured requirement, not a guess: at 512 MiB `TargetFeaturesEmitter.cpp`
# thrashed for the whole 90-minute budget, and at 1 GiB the sequence reached the
# thirteenth unit before the cache and memory pressure stopped it. The second
# boot is not a convenience: the binary runs on a fresh image but hangs when run
# in the boot that compiled it (KNOWN_ISSUES.md item 89).

