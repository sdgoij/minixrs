# Hyprland as a reference, not a base

Status: **evaluation**, feeding `.notes/gui.md`. Measured against the tree
under `.refs/hyprland/`, commit `a8d9a4e` (2026-11-08), shallow clone, no
subprojects.

## 1. Verdict

Hyprland does not replace the decision in `.notes/gui.md`, and it is not a
smaller undertaking than `cosmic-comp`: `src/` is 140,709 lines against
cosmic-comp's 72,000, in C++26, on a dependency set that is at least as
Linux-bound. It is a better reference than cosmic-comp was, for two reasons:
BSD-3-Clause licensing throughout the Hypr* ecosystem, and a layering that is
its own rather than a thin skin over smithay.

About a fifth of the tree is worth reading. The rest is protocol glue tied to
`libwayland-server`, X11, IPC, and its own configuration language.

## 2. Measurements

| | Hyprland | cosmic-comp |
|---|---|---|
| `src/` lines (`.cpp`/`.hpp`/`.h`) | 140,709 in 958 files | 72,000 |
| `hyprctl` + `hyprpm` + tests | 52,529 | |
| language | C++26 (own libs C++23) | Rust |
| licence | BSD-3-Clause | GPL-3.0 / MPL-2.0 |
| protocol core | `libwayland-server` (C) | `wayland-server` via smithay |
| backend | Aquamarine (separate lib) | smithay DRM/GBM |

Where the 140,709 lines go:

```
25,629  protocols     16,386  desktop        2,772  state
19,091  render        14,807  managers       2,419  pointer
18,123  config         9,663  layout         2,224  keybinds
 4,918  helpers        4,686  output         1,688  plugins
 3,664  xwayland       3,348  ipc            1,315  debug
```

Inside `render/`, the pass module is 1,553 lines and `gl` plus `shaders` is
7,442. Inside `layout/`, the algorithms are 7,207, of which the tiled family
is 5,729.

## 3. Why it cannot be a base

**It is a program, not a library.** `hyprland.pc` and the installed headers
exist for C++ plugins, and the plugin ABI is explicitly unstable. There is no
crate or shared library to depend on, so "use Hyprland" means forking it, and
the fork is twice the size of the cosmic-comp fork that already failed.

**The dependency wall is the decisive point.** From `CMakeLists.txt`:

- protocol and OS: `wayland-server>=1.22.91`, `wayland-protocols>=1.49`,
  `libinput>=1.29`, `xkbcommon>=1.11`, `libdrm`, `gbm`, `libeis`, `systemd`
  (optional)
- text and graphics: `pango`, `pangocairo`, `cairo`, `pixman`, `xcursor`,
  `lcms2`
- runtime: `Lua 5.5`, `glaze`, `re2`, `muparser`, `libcanberra`, `gio-2.0`,
  `udis86`
- build time: `glslang` and `hyprwayland-scanner`

Aquamarine adds `libseat`, `libudev`, `libinput`, `wayland-client`, `gbm`,
`libdrm`, `libdisplay-info` and `hwdata`. Every one of these has to exist on
minix before the first compositor line runs, and the seat, udev and libinput
trio is Linux session and device plumbing with no minix analogue: we have
`/dev/fb` and `/sbin/input` instead. A port also needs a C++26 toolchain and
libstdc++ in a userland that today has Rust and C.

**Its largest module is the one we can reuse least.** `src/protocols/` is
25,629 lines built on `wl_resource` and `wl_listener`, so it only runs if
`libwayland-server` is ported. That would replace `crates/wayland`, 6,062
lines of Rust that already works.

## 4. Where it beats cosmic-comp

- **Licence.** BSD-3-Clause across `Hyprland`, `Aquamarine`, `hyprutils`,
  `hyprgraphics` and `hyprtoolkit`, and it has been BSD-3 since the first
  licence commit in 2022. That is compatible with this project's GPL-2.0-only,
  so translating a named algorithm or copying a shader is legal with a
  provenance note. cosmic-comp's GPL-3.0 ruled that out regardless of effort.
- **No middle layer.** `render/`, `layout/` and `desktop/` are its own design
  rather than 135 smithay re-exports, so the ideas are readable and separable
  even though the code is not.

## 5. What to take

### 5.1 The render pass list (`src/render/pass/`, 1,553 lines)

`CRenderPass::add` accumulates elements for a frame. `simplify()` walks them
in reverse, subtracts each element's `opaqueRegion` from the remaining damage
and discards anything whose `boundingBox` no longer intersects it. `render()`
draws only what survives. `IPassElement` exposes `boundingBox`,
`opaqueRegion`, `undiscardable`, `needsLiveBlur` and `needsPrecomputeBlur`.

This is the whole of M2's "correct overlap and no corruption" gate in about
sixty lines of logic. Adopt the shape at M1 even though it is overkill there,
because retrofitting draw ordering after the fact is exactly the rework the
milestone gates exist to prevent.

In Rust this should be an enum plus an exhaustive `match`, not the
polymorphic hierarchy: the element list is closed, and one function that
sees every variant is easier to keep correct than twelve virtual overrides.

### 5.2 Renderer interfaces (`src/render/{Texture,Framebuffer,Renderbuffer,GLRenderer}.hpp`, `ElementRenderer.hpp`)

`ITexture`, `IFramebuffer`, `IRenderbuffer` and `IElementRenderer` separate
"what to draw" from GLES calls. This is the `Renderer` layer of `gui.md`, and
it is what allows the M1 GLES path and a later software path to coexist.

### 5.3 Layout algorithms (`src/layout/algorithm/`, 7,207 lines)

`CAlgorithm` owns one tiled and one floating algorithm per space and routes
by `target->floating()`. Each algorithm implements `newTarget`,
`removeTarget`, `recalculate`, `moveTargetInDirection` and
`getNextCandidate`. The algorithms themselves are small: dwindle 868, master
1,399, monocle 295, floating 311; the scrolling family is larger (2,014 plus
helpers). This maps directly onto two Rust traits and is the cheapest large
win in the whole tree.

### 5.4 Decorations (`src/render/decorations/`)

Border 235 lines, shadow 311, inner glow 174, group bar 562, plus a single
`DecorationPositioner` of 413. Decorations are objects attached to a window
with a desired extent and z-order, laid out in one pass. That is the model a
widget frame can reuse at M4.

### 5.5 Shaders (`src/render/shaders/glsl/`, about 50 files)

`#version 300 es`, so already GLES 3.00: `shadow.frag`, `border.frag`,
`rounding.glsl`, `quad.frag`, `tex300.vert`, `passthru.frag`, `surface.frag`
and the blur chain. This is the closest thing to drop-in reuse in the tree.
One catch: includes are resolved by glslang callbacks in `ShaderLoader.cpp`,
not by the GL extension, so we need a small textual `#include` resolver.

### 5.6 Smaller items

- animation curves (`src/animation/`, 815 lines) behind one
  `CAnimatedVariable`: bezier and spring
- seat and focus state for M3: `desktop/state/FocusState.cpp` 331,
  `desktop/state/ViewHitTester.cpp` 362, `managers/SeatManager.cpp` 898
- the window model split in `desktop/view/window/` (`Window.cpp` 2,006,
  `WaylandBackend.cpp` 448) between policy and protocol backend. The X11 half
  (`X11Backend.cpp`) is irrelevant to us, but the split is not.

### 5.7 Not worth reading

`protocols/` 25,629, `config/` 18,123, `xwayland/` 3,664, `ipc/` 3,348,
`plugins/` 1,688, `i18n/` 1,906. That is 54,000 lines, 38% of the tree, all
of it Linux integration or a configuration language we are not adopting.

## 6. What it does not solve

Hyprland has no widget toolkit. It draws borders, shadows and group bars, and
it renders text through Pango and Cairo, which is a hard dependency and means
it punted on the problem `gui.md` names as the place GUI projects die.

The separate `hyprtoolkit` (36,576 lines, C++23) does widgets, but it pulls
Pango, Cairo and Aquamarine as well, so it illustrates the cost of the
shortcut rather than offering one. For M4 and M5 Hyprland supplies nothing.

## 7. Consequences for the plan

- Keep §1 of `gui.md` unchanged.
- Fold the pass list and damage regions into M1 rather than leaving draw order
  implicit. This is the one substantive change that follows from here.
- M2's overlap gate becomes a test of `simplify()`: two windows, one opaque,
  one moved, expected damage region compared per frame.
- Provenance for translated algorithms and copied shaders goes in the file
  that takes them, as with `.refs/minix-3.3.0`.

## 8. Method

Shallow clone at the commit above, line counts from `find` plus `wc -l` over
`src/`, `hyprctl/`, `hyprpm/`, `tests/` and `hyprtester/`. Dependency and
language facts from `CMakeLists.txt` here and in Aquamarine. `hyprtoolkit`
was cloned only to measure it and was removed; it is not required to read
this note. The Hyprland tree is kept under `.refs/hyprland/` and ignored by
`.gitignore` alongside the other browsing clones.
