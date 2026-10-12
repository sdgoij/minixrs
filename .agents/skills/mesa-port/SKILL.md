---
name: mesa-port
description: Cross-building Mesa and libdrm for minix — the Wayland §6.10 phase-3 GL stack, its libc++ runtime, the cc-dso-minix DSO wrapper, and the surfaceless-EGL client, on all three arches. Use when running `just build-mesa`, `just libcxx` or `just gltriangle`/`just test-gltriangle`, when a Mesa/libdrm compile or link fails, when changing tools/build-mesa.py, tools/build-libcxx.py or tools/cc-dso-minix.py, or when working on the GL/EGL stack.
---

# Mesa + libdrm for minix

The GL stack is a port of Mesa 25.3.6 + libdrm 2.4.129 onto the port's C
toolchain. `.notes/wayland.md` §6.10 is the design; these are the recipes and the traps.

## The pieces

- `just build-mesa x86 --build` — `tools/build-mesa.py`: fetches the pinned
  sources under `target/{mesa,drm}-src`, builds libdrm into a prefix, then
  configures/builds Mesa through **meson in WSL**. The meson build trees are on
  WSL's *native* filesystem (`~/.cache/minixrs/mesa`) because meson rejects
  drvfs (`/mnt/c`) mtimes ("clock skew"); the cross file and logs live in
  `target/mesa/x86/`. `--stage` (also run after a build) copies the seven DSOs
  into `target/mesa/x86/lib/` under their guest sonames, because the Windows
  `build-x86` cannot read WSL's native tree.
- `just fetch-mesa <arch>` — installs the pinned DSOs (and the EGL/GLES2/KHR
  headers the client compiles against) from the `mesa-<pin>` release instead of
  building them, the way `just fetch-stage1` installs the toolchain. This is the
  cheap path CI and dev machines use; `just build-mesa` stays authoritative.
- **The pin is `tools/mesa_pin.py`.** It holds the Mesa and libdrm tags/commits;
  the release tag is derived from the commit pair, so an artifact can only match
  the pinned sources. `.github/workflows/mesa-release.yml` builds and publishes a
  pin's seven DSOs per arch once, `ci.yml`'s `mesa-pin` job decides whether that
  is needed, and only a pin bump (or a `cc-dso-minix.py` change) triggers a
  rebuild — same shape as the stage1/toolchain-release pair.
- `just libcxx <arch>` — builds libc++/libc++abi as one static `libstdc++.a`
  (`target/cxx/<arch>/minix-runtime/`), from `tools/build-libcxx.py` and
  `tools/libcxx-toolchain.py`'s generated cross file. Mesa is C++ (its GLSL
  compiler), so this is a prerequisite; per-arch, so the runtime matches the DSOs.
  It is linked into the DSOs statically, so it is not a separate released artifact.
- `tools/cc-dso-minix.py` — the `cc` meson drives: compiles C **and C++**
  (`--cxx`) to `-fPIC` objects and links minix **shared objects** against
  `libc.so`. It is the piece that grows a feature every time Mesa asks for one.
- `just gltriangle <arch>` — builds the surfaceless-EGL GLES2 triangle client
  (`tools/gl_triangle.c`, `tools/build-gltest.py`) and stages the DSOs.
- `just wlgl-x86` — `tools/build-wlgl.py` builds `/bin/wlgl`, the 3d-a client that
  presents a GL surface through the in-house Wayland compositor: `wlclient`'s
  protocol half plus `tools/wlgl_render.c` for the GL half. It is the tree's first
  *dynamically linked Rust* program, and the link is the interesting part (see the
  trap below).

## Traps that cost a session

- **A C header a C++ TU includes must carry `extern "C"`, or the C++ consumer mangles the name.** Then nothing defines it and the loader reports an unresolved symbol like `_Z16__errno_locationv` (or `_Z18pthread_mutex_lockP15pthread_mutex_t`). `tools/c-include` is mostly guarded already; `errno.h`, `pthread.h`, `sched.h` and `sys/file.h` were the ones that were not, and only the C++ build (`just libcxx <arch>`, then the Mesa relink) surfaced them. `tools/check-c-headers.py` is the contract check, not this.
- **The libc++ runtime is a separate build from Mesa's.** A `tools/c-include` fix
  reaches libc++ only after `just libcxx <arch>`; Mesa alone will still link the old
  `target/cxx/<arch>/minix-runtime/libstdc++.a` and keep the mangled reference.
  Rebuild libc++, then relink Mesa.
- **lld does not re-export hidden symbols through a version script or
  `--dynamic-list`; GNU ld does.** Mesa sets `gnu_symbol_visibility: 'hidden'`
  and relies on its `.sym`/`.dyn` export lists, so under lld the DSOs export
  *nothing* and every consumer link fails with `undefined symbol`. Fix: put
  `-fvisibility=default` in Mesa's `c_args`/`cpp_args` (our `c_args` land after
  Mesa's `-fvisibility=hidden`, so the later flag wins) and let the version
  script do the limiting.
- **A `cc` wrapper must preserve link flag/input order.** Hoisting all `-Wl,`
  flags ahead of all inputs moves `-Wl,--whole-archive … -Wl,--start-group`
  away from the archives they wrap, so `libdri.a` is never extracted and the
  version script has nothing to export. `link()` walks the original argv and
  compiles sources in place.
- **`-Wl,--version-script` / `--dynamic-list` are separate tokens.** meson passes
  the option and its file as two arguments; a classifier that treats the bare
  file as an input compiles it. `cc-dso-minix.py`'s `WL_OPT_TAKES_VALUE` consumes
  the next token.
- **The libc++ runtime must be `-fPIC`.** It is a static archive linked into
  Mesa's shared objects; built `-fno-pic` it fails with `R_X86_64_PC32 cannot be
  used against symbol …`.
- **libc++'s objects carry a `-lpthread` dependency specifier** (`.deplibs`); lld
  follows it and fails. `cc-dso-minix.py` drops one-line `INPUT ( libc.so )`
  shims named `libpthread.so`, `libm.so`, `libdl.so`, `librt.so` beside `libc.so`.
- **C++ needs `-fno-exceptions`.** The port has no unwinder, so libc++abi is
  built without exceptions; Mesa's C++ must not emit `__cxa_throw` /
  `_Unwind_Resume`. `-fno-rtti` stays off (libc++abi's `private_typeinfo` uses
  `dynamic_cast`).
- **Include order is libc++ → c-include → the generated `__config_site`.** Use
  `-I`, not `-isystem`, for the libc++ dirs: clang searches every `-I` before
  every `-isystem`, so `-isystem` lets the C `<string.h>` win and libc++'s
  `<cstring>` wrapper fails its own header check.
- **`-D__managarm__`, `-DEGL_NO_PLATFORM_SPECIFIC_TYPES`, `-DNO_REGEX`,
  `-Dshader-cache=disabled`, `-Dlmsensors=disabled`, `-Dzlib:tests=disabled`.**
  Mesa's `detect_os.h` has no minix case (managarm is the closest POSIX-on-
  microkernel); `EGL/eglplatform.h` `#error`s on an unnamed OS and surfaceless
  needs no native types; the rest drop the on-disk cache (`nftw`), sensors,
  regex and zlib's test programs.
- **`LLVM_ENABLE_THREADS` needs `LIBCXX_HAS_PTHREAD_API=ON`.** The pthread API
  auto-detection fails in a cross build, leaving `_LIBCPP_HAS_THREAD_API_PTHREAD
  0` and `std::mutex` undefined. Enabling it needs the port's `pthread_barrier_*`
  / `pthread_rwlock_*` / `pthread_condattr_*`, which the runtime relies on.
- **`abort`/`exit` must be `noreturn`**, or libc++'s `system_error.cpp`
  (compiled with `-Werror=return-type`) trips on a function whose only exit is
  `std::abort()`.
- **Mesa builds its shader text with the *port's* `printf`, so a `printf` gap is a
  corrupted shader, not just a garbled log.** `virgl_encode_shader_state` calls
  `tgsi_dump_str`, which formats the TGSI into the string sent to the host through
  `vsnprintf`. Mesa labels each instruction `"% 3u"`; with the space flag
  unimplemented the literal `% 3u` reached the wire and the host's parser refused
  the stream (`Error assigning TGSI`, `context 1 failed to dispatch CREATE_OBJECT:
  22`). `vsnprintf`'s return must also be the *field* length, because `tgsi_dump`'s
  `str_dump_ctx_printf` advances a write cursor by it.
- **A program that loads Mesa must be *dynamically linked*, and linking a dynamic
  Rust one needs care.** `dlopen` is `#[cfg(feature = "so")]` in `minix-libc`: a
  static image has no loader, so `dlopen` returns NULL and Mesa's DSOs never come
  in (the C `gltriangle`/`cdyn-demo` work because C has no lang items to collide).
  `/bin/wlgl` is therefore linked with the arch's C crt0, `minix-user.ld`,
  `-Bdynamic -l:libc.so -l:libEGL.so.1 -l:libGLESv2.so.2` and
  `--dynamic-linker=/libexec/ld.so`. Three things bite: `minix-rt`'s `_start`
  collides with the C crt0's, so the crt0's is renamed (`-D_start=minix_crt0_entry`
  while compiling the `.S`, then `-e minix_crt0_entry`, since the linker script still
  says `ENTRY(_start)`); rustc needs the crate search path as
  `-L dependency=<release>/deps`, because `-C link-arg=-L` only reaches the linker
  (`E0460` otherwise); and the stub must not define its own `#[panic_handler]`
  (`E0152`, duplicate lang item with `minix-rt`). The GL half is
  `#[cfg(target_os = "minix")]` in `crates/userland/src/wlclient.rs`, because
  `wlgl_render` is a C object present only in the wlgl link and a *host* build of
  the crate must not reference it.

## The runtime gate

`just test-gltriangle-<arch>` boots the split pair with `/bin/gltriangle` and the
seven Mesa DSOs in the *system* image (`MINIXFS_BLOCKS=16384`, ~64 MiB — the two
copies of `libgallium` alone are ~33 MiB, one named by `libEGL`/`libGLESv2` in
`DT_NEEDED` and one `dlopen`ed as `swrast_dri.so`) and drives
`tools/smoke/gltriangle.tsv`, which wants the client's `gltriangle: pass` line.
It passes on x86_64, riscv64 and aarch64: EGL 1.5 / GLES 3.1 on `softpipe`, a red
centroid and a black corner. The split (`system-image-<arch>`) is what made it
fit — the image ceiling is gone, so a root past the 16 MiB `RAMDISK_IMAGE_SIZE` is
a system image, not a blocker. It needs `just libcxx <arch>` and `just build-mesa
<arch> --build` first, and CI's `gltriangle` job runs it for every arch and
publishes the seven DSOs as `mesa-<arch>`, which the release injects into the
system image it publishes.

`just test-gltriangle-virgl-x86` is the 3c-3 gate: the same client on a GL device
(`-device virtio-gpu-gl-pci,blob=on`), which takes the `virgl` driver on the render
node and asserts `GL_RENDERER=virgl` as well as the readback. It needs a GL-capable
dev host (headless CI has none) and is not a CI gate.

`just test-wlgl-x86` is the 3d-a gate: `/bin/wlgl` renders the triangle on the render
node, commits it to `/sbin/wlserver` as a `wl_shm` surface, and reads the composited
frame back from `/dev/fb`, asserting `GL_RENDERER=virgl` too. Like 3c-3 it needs a
GL-capable dev host and is not a CI gate. It shares `/dev/fb`'s `mmap` path with
`/bin/wlclient`, so a break there fails both gates with a black framebuffer.

## Validating a change

```
just libcxx <arch>                  # the C++ runtime (x86|riscv64|aarch64)
just build-mesa <arch> --build       # compiles everything; 0 FAILED in the log
just fetch-mesa <arch>               # or the pinned DSOs, without building
just test-gltriangle-<arch>          # boots the triangle through surfaceless EGL
just test-gltriangle-virgl-x86       # the 3c-3 gate: the same triangle on virgl (GL dev host)
just test-wlgl-x86                   # the 3d-a gate: the triangle through wlserver to /dev/fb (GL dev host)
just check                           # host clippy + header contract + physmap
```

Read `target/mesa/x86/ninja.log` for the real errors: ninja interleaves progress
lines with compiler output, so grep for `^FAILED:` and the `error:` lines rather
than trusting the tail. The Meson build dir is `~/.cache/minixrs/mesa/mesa/x86`.
