---
name: mesa-port
description: Cross-building Mesa and libdrm for minix — the Wayland §6.10 phase-3 GL stack, its libc++ runtime, the cc-dso-minix DSO wrapper, and the surfaceless-EGL client. Use when running `just build-mesa`, `just libcxx-x86` or `just gltriangle-x86`, when a Mesa/libdrm compile or link fails, when changing tools/build-mesa.py or tools/cc-dso-minix.py, or when working on the GL/EGL stack.
---

# Mesa + libdrm for minix

The GL stack is a port of Mesa 25.3.6 + libdrm 2.4.129 onto the port's C
toolchain. `WAYLAND.md` §6.10 is the design; these are the recipes and the traps.

## The pieces

- `just build-mesa x86 --build` — `tools/build-mesa.py`: fetches the pinned
  sources under `target/{mesa,drm}-src`, builds libdrm into a prefix, then
  configures/builds Mesa through **meson in WSL**. The meson build trees are on
  WSL's *native* filesystem (`~/.cache/minixrs/mesa`) because meson rejects
  drvfs (`/mnt/c`) mtimes ("clock skew"); the cross file and logs live in
  `target/mesa/x86/`. `--stage` (also run after a build) copies the seven DSOs
  into `target/mesa/x86/lib/` under their guest sonames, because the Windows
  `build-x86` cannot read WSL's native tree.
- `just libcxx-x86` — builds libc++/libc++abi as one static `libstdc++.a`
  (`target/cxx/minix-runtime/`), from `tools/libcxx-toolchain.py`'s generated
  cross file. Mesa is C++ (its GLSL compiler), so this is a prerequisite.
- `tools/cc-dso-minix.py` — the `cc` meson drives: compiles C **and C++**
  (`--cxx`) to `-fPIC` objects and links minix **shared objects** against
  `libc.so`. It is the piece that grows a feature every time Mesa asks for one.
- `just gltriangle-x86` — builds the surfaceless-EGL GLES2 triangle client
  (`tools/gl_triangle.c`, `tools/build-gltest.py`) and stages the DSOs.

## Traps that cost a session

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

## The runtime gate is blocked on the root filesystem

`tools/smoke/gltriangle.tsv` and the client exist, but a guest cannot yet carry
them: the DSOs are ~33 MiB — **two** copies of the 16.5 MiB `libgallium`, because
`libEGL`/`libGLESv2` name it in `DT_NEEDED` *and* EGL dlopens it as
`swrast_dri.so` — and a root image past the 16 MiB `RAMDISK_IMAGE_SIZE` default
stalls the boot before `wserver` reports ready (verified embedded *and* as a
virtio-blk root; `MINIXFS_EXTRA` can put the DSOs in the disk image, but the
image is the same blob either way). Raise the root-fs ceiling (MFS) first; the
boot recipe is in the `gltriangle-x86` doc comment.

## Validating a change

```
just build-mesa x86 --build          # compiles everything; 0 FAILED in the log
just libcxx-x86                      # the C++ runtime
just check                           # host clippy + header contract + physmap
```

Read `target/mesa/x86/ninja.log` for the real errors: ninja interleaves progress
lines with compiler output, so grep for `^FAILED:` and the `error:` lines rather
than trusting the tail. The Meson build dir is `~/.cache/minixrs/mesa/mesa/x86`.
