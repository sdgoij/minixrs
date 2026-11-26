# DOOM: doomgeneric on minix

Status: **runs**. The shareware episode boots to its attract demo on x86_64,
drawn into `/dev/fb`; the console drives the menus, and `cargo xtask gate doom
--arch x86` holds the first frame to a read-back of the live mapping.

Doom is the port's first game: about 80 translation units of 1993-era C, built
by the same clang + LLD + `libc.a` line as `c-direct`.

## 1. The pieces

| Piece | Where |
|---|---|
| Engine sources | pinned in `tools/pins.ini`, checked out at `target/doom-src` |
| Shareware IWAD | `target/doom-data/doom1.wad`, held to the digest in the same file |
| Platform layer | `tools/doom/doomgeneric_minix.c` |
| Builder | `cargo xtask build doom <arch>` (`crates/xtask/src/build/doom.rs`) |
| Recipes | `just doom-x86`, `just prepare-doom-x86` |
| Playing | `just run-doom-x86`, then `/bin/doom` at the shell |
| Gate | `cargo xtask gate doom --arch x86`, scenario `tools/smoke/doom.tsv` |

`doomgeneric` is a port of the Doom source with one small interface between the
engine and a platform (`DG_Init`, `DG_DrawFrame`, `DG_SleepMs`, `DG_GetTicksMs`,
`DG_GetKey`, `DG_SetWindowTitle`). Upstream's platform files each own a `main`
and a host window; minix has neither, but it has a framebuffer device and a
console, which is the whole of what the interface asks for.

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

The console is put in raw mode (VMIN = VTIME = 0, so `DG_GetKey` polls and
never blocks). A terminal reports presses, not releases, while Doom's input loop
expects both, so a key is held for 250 ms after its last repeat byte and then
released; with the terminal's own auto-repeat a held key stays held, and a tap
is a press and a release. `ESC [ A`..`D` become the arrow keys (modifier
parameters are skipped), Escape is the menu, Space is use, Ctrl-C is fire (safe:
raw mode clears `ISIG`).

`-minix-exit` makes the platform layer leave once it has printed its verdict, so
the gate gets its shell prompt back. A player does not pass it. The gate types
the command paced, because an unpaced long line lost a byte on the serial
console (`-iwad` arrived as `-iwa1ad`), and a paced step is retried until the
guest echoes it whole.

`just run-doom-x86` boots the image in an SDL window with the shell on stdio, so
the same terminal that types `/bin/doom` is the console Doom reads. The recipe is
separate from `just run` and `just desktop` because those rebuild the system
image, which would overwrite the disk that carries `/bin/doom` and the IWAD. It
has no `virtio-tablet-pci`: doomgeneric starts with `usemouse = 0`, and that
device with SDL on stdio wedges QEMU's main loop (`KNOWN_ISSUES` 95).

## 5. Limits

- The gate is x86 only. The platform layer is arch-neutral and the virtio-gpu
  backend already flushes, so RISC-V and AArch64 should work; nobody has gated
  them yet.
- No sound: the engine is built without `FEATURE_SOUND`, so `dummy.c` provides
  the mixer stubs.
- No mouse: doomgeneric starts with `usemouse = 0`, and the tablet device is not
  read.
- The 320x200 image is scaled with square pixels (960x600), not to Doom's 4:3
  aspect; the difference is a few percent and it keeps the scale an integer.
