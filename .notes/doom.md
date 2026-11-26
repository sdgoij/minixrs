# DOOM: doomgeneric on minix

Status: **runs**, and plays, on x86_64, RISC-V64 and AArch64. The shareware
episode boots to its attract demo, drawn into `/dev/fb`; `cargo xtask gate doom
--arch <arch>` holds the first frame to a read-back of the live mapping, and on
every arch `doom-kbd` quits through the menu and `doom-save` saves a game that
survives a reboot.

Doom is the port's first game: about 80 translation units of 1993-era C, built
by the same clang + LLD + `libc.a` line as `c-direct`.

## 1. The pieces

| Piece | Where |
|---|---|
| Engine sources | pinned in `tools/pins.ini`, checked out at `target/doom-src` |
| Shareware IWAD | `target/doom-data/doom1.wad`, held to the digest in the same file |
| Platform layer | `tools/doom/doomgeneric_minix.c` |
| Builder | `cargo xtask build doom <arch>` (`crates/xtask/src/build/doom.rs`) |
| Recipes | `just build-doom x86` (`doom-x86` is the same build), `just prepare-doom-x86` (the system image with Doom in it) |
| Playing | `just run-doom-x86`, then `/bin/doom` at the shell; click the window and play. `just run <arch>` has it too once `build-doom` has run |
| Gates | `cargo xtask gate doom|doom-kbd|doom-save --arch <arch>`, on all three arches (frame and read-back, a device key and the quit, a save across a reboot) |

The engine is not in the kernel's embedded root: the shareware IWAD is 4 MiB and
the embedded ramdisk window is 16 MiB, so Doom goes in the *system* image
(`system-image-<arch>`, the disk a `run` recipe boots) and the release's split
image. Those recipes include `/bin/doom` and the IWAD when `just build-doom
<arch>` has put them under `target/`, the same conditional rule `/bin/bash`
follows, so a plain image build depends on neither. `ci.yml`'s `doom` job builds
the pin on all three arches, runs the three gates and publishes the binaries;
`image-release.yml` restores them before building the system image the release
publishes.

`doomgeneric` is a port of the Doom source with one small interface between the
engine and a platform (`DG_Init`, `DG_DrawFrame`, `DG_SleepMs`, `DG_GetTicksMs`,
`DG_GetKey`, `DG_SetWindowTitle`). Upstream's platform files each own a `main`
and a host window; minix has neither, but it has a framebuffer device and an
input ring, which is the whole of what the interface asks for.

The engine is compiled with upstream's `SRC_DOOM` list minus the platform file,
with `-DNORMALUNIX -DLINUX` (the family its sources branch on) and
`-DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200` (its own frame buffer is the
native size). Its warnings are suppressed: they are not this port's to fix, and
a wall of them would hide the one that is.

## 2. The frame path

The engine draws 320x200 into `DG_ScreenBuffer` as XRGB8888 in byte order
`B, G, R, X`, which is exactly what the fb server's 1024x768 mode carries
(`drivers/video/fb.rs`). So the frame is an integer nearest-neighbour scale
(3x here, 960x600) copied a row at a time into a mapping of `/dev/fb`, with the
bars around the window cleared once at start-up. `FBIOFLUSH` follows every
frame; it is a no-op on bochs and the push the virtio-gpu backend needs.

The first frame with more than a handful of distinct pixels is checked before it
is trusted: one pixel is read back out of the mapping and compared with the
source pixel just written. That is the same property `/bin/fbmmap paint` asserts,
applied to the engine's output, and it is what makes the gate's `doom: PASS` a
statement about the live framebuffer rather than about a private buffer.

## 3. The IWAD

The shareware `doom1.wad` (4,196,020 bytes) is freely redistributable but is not
in the engine's repository, so the pin is its SHA-256: a mirror that changes the
file fails the build instead of rendering a different game. `doom.wad.url` is
only where the digest is fetched from.

The engine is built without `ORIGCODE`, so its IWAD search is the working
directory alone (`FILES_DIR` is `"."`). The image installs the WAD at
`/usr/share/games/doom/doom1.wad`, and `DG_Init` makes that the working
directory, which is why `/bin/doom` needs no arguments. Enabling `ORIGCODE` for
the Unix search paths would also switch on unrelated vanilla code in
`d_main.c`, `d_loop.c`, `w_wad.c` and `m_config.c`, which is a larger change
than the search path is worth.

## 4. Input

Two sources feed the engine, and the one that matters for play is `/dev/kbd`, the
input server's HID ring (major 20, `crates/servers/src/input.rs`). An event is
`{page, code, press}`, 8 bytes little-endian: a HID usage code and both edges, so
a key needs no synthesis and no escape-sequence parsing. A window's keyboard
reaches the ring through QEMU's PS/2 controller. The compositor reads the same
ring from its own cursor, which is the point of the server's per-reader design:
a second consumer loses nothing. The platform layer prints `doom: kbd PASS` the
first time an event arrives from it, and the `doom-kbd` gate asserts that line
after QMP injects Escape, which is a real device event.

Letters are lowercased, the way every upstream backend delivers them (`key =
tolower(key)` in `doomgeneric_sdl.c`). Doom compares letters literally: the quit
confirmation is `key_menu_confirm = 'y'` and the cheat sequences are lowercase,
so an uppercased key matches neither.

When `/dev/kbd` opens it is the only input source: the console is left in
canonical mode and never read, so the terminal behind `-serial stdio` stays the
shell's and a keystroke typed at it does not reach the game. The console is the
fallback when the device cannot be opened, and `-minix-console` adds it back for
a debug session with no window. In that mode it is put in raw mode (VMIN =
VTIME = 0, so `DG_GetKey` polls and never blocks), and since a terminal reports
presses and not releases, a console key is held for 250 ms after its last repeat
byte and then released; with the terminal's own auto-repeat a held key stays
held, and a tap is a press and a release. `ESC [ A`..`D` become the arrow keys
(modifier parameters are skipped).

On riscv64 and aarch64 the keys arrive as Linux keycodes from the virtio
keyboard, and `keycode_to_usage` (`crates/drivers/src/input/virtio_input.rs`)
translates them to HID usages. That table had letters, digits, the arrows,
Enter, Backspace, Space, punctuation and the modifiers, and stopped there:
Escape, Tab, the function keys and the navigation block fell through as
unmapped and the ring never saw them. x86 never noticed, because the PS/2 path
delivers HID usages directly. The table now carries them; the `doom-kbd` gate on
those arches is what caught it (Escape is the menu key).

Doom's own bindings apply: Ctrl fires, Space is use, Shift runs, Alt strafes,
Escape is the menu.

Quitting leaves through the engine's exit hooks. `I_Quit` walks them and, built
without `ORIGCODE`, returns to the game loop rather than leaving the process; one
of the hooks, `D_Endoom`, calls `exit(0)` itself (after an ENDOOM screen that is a
stub here), so the menu's Quit Game does leave. The platform layer prints
`doom: exit` from an `atexit` handler, which both paths run, and registers its own
`I_AtExit` hook only as the fallback for when `D_Endoom` declines
(`-noendoom`, testcontrols, the screensaver).

`-minix-exit` makes the platform layer leave once it has printed its verdict, so
the `doom` gate gets its shell prompt back. A player does not pass it. That gate
types the command paced, because an unpaced long line lost a byte on the serial
console (`-iwad` arrived as `-iwa1ad`), and a paced step is retried until the
guest echoes it whole.

On wasm the walk could not complete. `G_CheckDemoStatus` is a `boolean(void)`
registered through `(atexit_func_t)`, a `void(void)` hook type; a native call
ignores the unused return, but wasm's `call_indirect` checks the signature, so the
first hook call trapped with "function signature mismatch" and took the guest
down. `-minix-exit` calls `exit` directly and never walked the hooks, so the gate
never saw it. The module's link line wraps `I_AtExit` (its own signature every
caller agrees on) and the wasm shim hands the real one a nullary trampoline for
that single registration, `tools/doom/doomgeneric_minix_wasm.c`; the same family
as the `atexit` trampoline in `crates/minix-libc` and procfs's callback tag
(`.notes/procfs.md`). The page harness now quits through the menu, so the walk is
exercised.

`just run-doom-x86` boots the image in an SDL window with the shell on stdio for
command typing; the game itself takes the window's keyboard through `/dev/kbd`,
so the terminal stays free. The recipe is separate from `just run` and `just
desktop` because those rebuild the system image, which would overwrite the disk
that carries `/bin/doom` and the IWAD. It has no `virtio-tablet-pci`:
doomgeneric starts with `usemouse = 0`, and that device with SDL on stdio wedges
QEMU's main loop (`KNOWN_ISSUES` 95).

## 5. Saves and errors

With `configdir` set to `"."`, `M_GetSaveGameDir` calls `mkdir("./.savegame/")`
and writes `./.savegame/temp.dsg`, then renames it to `doomsav0.dsg`. That one
call found a VFS bug: `last_dir` resolved the parent of any path containing a
slash from the process root, so the directory was created as `/.savegame` and
the save failed into `/tmp/recovery.dsg`. `last_dir` now takes `fp_cdir` for a
relative path and `fp_rdir` only for one that starts with `/`
(`crates/servers/src/vfs/path.rs`). A second boot, after `sync`, shows
`.savegame/doomsav0.dsg`, and `cargo xtask gate doom-save` is that check:
New Game, save slot 0, quit to the shell, list the file, `sync`, boot again,
list it again. Reverting the `last_dir` fix makes it fail with the engine's
`Failed to open savegame file` and its `/tmp/recovery.dsg` fallback.

`I_Error` probes for a desktop error box with
`system(ZENITY_BINARY " --help >/dev/null 2>&1")`, and `system()` forks
`/bin/sh -c`. Until the shell honoured `-c` it started an interactive prompt on
the console instead, which is how a fatal error left the guest flooding `# `
and needing a hard kill; the shell now runs the operand and exits. zenity is
not in the image, so the probe reports the missing binary and the game exits;
`-nogui` skips the probe entirely.

A QEMU that is killed does not flush MFS's write-back cache, so a directory
entry can reach the device while its inode does not. `sync` in the guest before
killing it, or inspect the disk only after a clean exit; otherwise a new
directory shows in the listing and cannot be resolved.

## 6. Limits

- No sound: the engine is built without `FEATURE_SOUND`, so `dummy.c` provides
  the mixer stubs.
- No mouse: doomgeneric starts with `usemouse = 0`, and the tablet device is not
  read. The keyboard is the window's, through `/dev/kbd`.
- The 320x200 image is scaled with square pixels (960x600), not to Doom's 4:3
  aspect; the difference is a few percent and it keeps the scale an integer.
- `doom-save` drives the menu with QMP keys and boots twice, so it takes about
twice as long as `doom-kbd`. The second boot mounts read-only, because the
first was not unmounted; it only reads the file.
- The single-file release image (`minix-x86.elf`) does not carry Doom: it is
the 20 MiB embedded root, and the WAD alone would take a fifth of it. The
split `minix-x86-system.img` is where the game ships.
