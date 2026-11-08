# Own GUI stack: plan

Status: **proposal**. Supersedes the smithay / `cosmic-comp` north star in
`.notes/wayland.md`. That document's analysis of the OS primitives Wayland
needs (local sockets, fd passing, a scalable readiness API, shared buffers)
still stands; only the choice of who sits on top changes.

## 1. Decision

Build the desktop stack ourselves, on the graphics stack that already works:
Mesa, EGL and GLES.

Explicitly not used: smithay, `cosmic-comp`, iced, libcosmic, or any of the
desktop crates around them.

The reasons are measured, not aesthetic:

- `cosmic-comp` is 72k lines of Rust and its public shape is smithay's API: 135
  distinct `smithay::` paths, of which 99 are `reexports` (`wayland_server`,
  calloop, drm, gbm, reis). It is a program, not a crate; it cannot be a
  dependency.
- A "drop-in replacement" for smithay is therefore not a thin layer. Behind
  those 135 paths sit roughly twenty Wayland protocol state machines, the
  `wayland_server` crate, a GL renderer and a DRM backend: 40 to 60k lines that
  must be protocol-exact.
- Fitting `cosmic-comp` to minix means either deleting its backends (what was
  attempted once: 24.7k lines removed, 105 files, and unmaintainable) or
  maintaining a shim against an API we do not control.
- The licence path is unclear at best: COSMIC is GPL-3.0 and libcosmic is
  MPL-2.0 against this project's GPL-2.0-only, and the assets are the part where
  that matters most.

What we keep and build on:

- The in-house Wayland server (`/sbin/wlserver`) and `crates/wayland`.
- `wl_shm` and dmabuf buffer passing, already exercised by the existing clients.
- The EGL / GLES composition probes as the reference for the render path:
  `tools/wlgl_render.c`, `tools/wlcompose_render.c`,
  `tools/wldmabufgl_render.c`.
- The input server (`/sbin/input`) and `/dev/fb`.
- Mesa (MIT). Nothing else is load-bearing.

Hyprland was measured as the remaining candidate and is not a base either:
`src/` is twice the size of `cosmic-comp`, in C++26, on a larger Linux-only
dependency set. Its licence and its layering make it the better reference,
and `.notes/hyprland.md` records what to take from it.

## 2. Layers and who owns them

| Layer | Contents | Owner |
|---|---|---|
| Display server | Wayland dispatch, surfaces, buffers, protocol state | ours |
| Renderer | EGL + GLES, render to texture/BO, present via dmabuf or `wl_shm` | ours |
| Scene | rectangles, textures, layers, damage, transform | ours |
| Widgets | layout, hit testing, focus, theme, clipboard | ours |
| Text | glyph rasterizer, line breaking, shaping subset | one small crate or ours |
| Input | seat, keyboard map, pointer, cursor | ours (`/sbin/input`) |
| Apps | terminal, files, launcher, settings | ours |

## 3. Protocol surface: the minimum

`wl_display`, `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat` (plus
keyboard and pointer), `xdg-shell`, `layer-shell` for the panel, and
`linux-dmabuf` for GPU clients. Later, only with a consumer: `wp_viewporter`,
`fractional-scale`, `xdg-decoration`.

Out of scope: Xwayland, X11 entirely, text-input/IME, screencopy, session-lock,
primary selection, drm-lease, and anything only a ported Linux desktop would
ask for.

## 4. Milestones, each with a gate

**M1, renderer.** A quad and one line of text drawn by our code into a buffer,
presented through the existing path, visible in a guest screenshot.

**M2, window server.** Two clients, two windows, correct overlap and no
corruption. Gate: a scenario in `tools/smoke/` with a screenshot.

**M3, input.** Focus follows the window under the pointer, the keyboard reaches
the focused client, and pointer motion is interpolated across a frame. Gate: a
client that reports what it received.

**M4, widgets.** Button, text field and list, with focus and key handling. Gate:
a demo app that uses all three.

**M5, shell.** Panel plus launcher plus one app, booting to a usable session.
Gate: a scenario that clicks the launcher and starts the app.

**M6, text and scaling.** Shaping beyond ASCII, HiDPI, and only then IME. This is
the last milestone because it is where the unbounded work lives.

## 5. Rules for dependencies

- A new crate is allowed only if it is small, MIT or Apache-2.0 licensed, and
  builds on minix unmodified. Otherwise we write it.
- No forks. If an upstream crate needs a minix arm, the patch goes upstream
  first; a fork is a last resort recorded as a cost.
- Mesa stays the only large third-party component. Fonts are OFL; the rasterizer
  is the one place a small crate is acceptable.

## 6. Non-goals

- Running unmodified COSMIC, GTK or Qt applications.
- Any X11 compatibility layer.
- A drop-in smithay, or any reimplementation of somebody else's toolkit API.
- Accessibility, IME and multi-seat before M6.

## 7. Risks

- **Text** is where GUI projects die. Keep it to one rasterizer and a line
  breaker until an application demands more.
- **Present path**: `wl_shm` copies through the filesystem; dmabuf avoids that
  but the import path is GPU-specific. Decide per client, and measure before
  optimising.
- **Input** is the least proven part of the existing stack. M3 exists to find
  that out early.
- **Scope**: a widget toolkit has no natural stopping point. The milestone gates
  are the stop.

## 8. Open decisions

- Which rasterizer: `swash`, `ab_glyph` or `fontdue`, or our own.
- Whether the panel speaks `layer-shell` or a private protocol on our server.
- Whether the default present path is `wl_shm` or dmabuf, and what that means
  for clients that ship both.

## 9. Reference implementations

Hyprland is the design reference, not a base: commit `a8d9a4e`, measurements
and reasoning in `.notes/hyprland.md`. Four pieces of it are BSD-3-Clause and
worth translating or copying:

- **Render pass list and damage regions** (`src/render/pass/`, 1,553 lines).
  Elements accumulate per frame; `simplify()` walks them in reverse,
  subtracting each one's opaque region from the damage and dropping the
  rest. Adopt the shape at M1. Draw order decided ad hoc gets rewritten at
  M2, which is the rework the gates exist to prevent.
- **Renderer interfaces** (`src/render/{Texture,Framebuffer,Renderbuffer}.hpp`,
  `ElementRenderer.hpp`): texture, framebuffer and element-renderer traits,
  so the scene is independent of GLES calls.
- **Layout algorithms** (`src/layout/algorithm/`, 7,207 lines; dwindle 868,
  master 1,399, monocle 295, floating 311): one tiled and one floating
  algorithm per space behind a small interface, which maps onto two traits.
- **GLSL ES 3.00 shaders** (`src/render/shaders/glsl/`): shadow, border,
  rounding, and the quad and texture pair. Their includes are resolved by
  glslang, so ours need a textual resolver instead.

Provenance goes in the file that takes the code, as with `.refs/minix-3.3.0`.

## 10. Progress

Reached, each with the gate that holds it:

| Milestone | What runs | Gate |
|---|---|---|
| M1 | `/bin/gui`: a quad and a line of text, drawn and presented | `just test-gui-x86` |
| M2 | `/bin/wloverlap`: two windows, one recommit, no corruption | `just test-wloverlap-x86` |
| M3 | `/bin/wlpointer` and `/bin/wlmotion`: focus follows the pointer, motion is interpolated | `just test-wlpointer-x86`, `just test-wlmotion-x86` |
| M4 | `/bin/wldemo`: a text field, a list and a button | `just test-wldemo-x86` |
| M5 | `/bin/session` starts `/etc/session` after the shell's prompt; `/bin/wlpanel` launches an app | `just test-rc-x86`, `just test-wlpanel-x86` |

M6 has not started. The panel speaks `zwlr_layer_shell_v1`, so the second open
decision is taken; the rasterizer and the default present path are not.
