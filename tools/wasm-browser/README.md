# The system in a browser tab

MINIX/Rust running in a page: the kernel, the servers and the shell, as WebAssembly, driven from
JavaScript by an engine that stops the guest between syscalls so the tab stays interactive.

```sh
sh tools/wasm-browser/build.sh     # build and stage the artifacts
node tools/wasm-browser/serve.js  # then open http://127.0.0.1:8080/
```

The server exists because a wasm module cannot be fetched from `file://` (the origin is opaque, so
the fetch is refused). It serves this directory and binds to loopback; nothing else is needed — no
bundler, no dependencies, no network.

Any static server will do, and that is why the modules here are `.js` rather than `.mjs`. A module
script is fetched under strict MIME checking: if the server answers `text/plain` the browser
refuses it *before it runs*, the page stays blank, and the only explanation is one line in the
browser's own console. `.mjs` is the extension servers most often get wrong; `.js` is the one they
almost all map to a JavaScript type, and `package.json` next to them makes Node agree that the
contents are ES modules. `serve.js` cannot get it wrong, and `index.html` creates the module script
itself so that a load failure explains itself on the page rather than only in the console.

## What it runs

The same three artifacts the check harness runs, from `tools/wasm-servers/build.sh`: the kernel
instance, the servers as one module, and the boot filesystem image. The page boots the same system
the harness does — DS, RS, PM, the RAM disk, VM, MFS, a device-less virtio-blk, devman, VFS and the
tty — INIT execs `/bin/sh` out of the image, and the shell forks and execs for a command it cannot
answer itself.

The specs the page boots are `SYSTEM_SPECS` in `host.js`. The harness starts four more instances
it does not share: two DS clients and two program probes. Those are test apparatus, and they are
the only difference between what the checks boot and what the page boots.

## The files

| File | What it is |
|---|---|
| `index.html` | the page: a screen, a status line, and a panel for host reports |
| `page.js` | the DOM front end — the keyboard, the renderer, and the pump loop |
| `host.js` | the engine: instances, the copy seam, exec, fork, and the dispatch loop |
| `terminal.js` | the renderer's model: bytes in, lines out, with a cursor |
| `run.js` | drives the engine from Node with scripted keystrokes (5 checks) |
| `page.test.js` | the server's MIME types, then `page.js` under a stub DOM (13 checks) |
| `serve.js` | a static server, for `file://`'s sake |
| `build.sh` | stages what the page fetches, via `tools/wasm-servers/build.sh` |

## Why an idle prompt is interesting

The shell retries `read(0)` in user mode when there is nothing to read, and on this port the tty's
own `do_read` does the same — an idle console is a *spin*, not a block
(`PORTING_PLAN.md` finding 32). That has two consequences a browser makes unavoidable, and they
are what `host.js`'s slice mechanism exists for:

- The guest never hands control back on its own, so a page that let it run would freeze on the
  first idle prompt. `pump()` therefore ends a slice after a fixed number of syscalls and unwinds
  the instance through Asyncify — the same mechanism a blocked syscall uses. The instance is not
  paused from outside; it is stopped *between* syscalls with its state serialised.
- The tab must not burn a core while the guest waits. `sliceWasSpinOnly()` reports whether a slice
  did nothing but retry the console read with an empty queue — which is what "the guest is waiting
  for you" looks like from outside — and the page parks on it: no slices at all until a keystroke,
  with a two-second safety valve in case the reading is ever wrong.

Interrupting an idle guest also found a cross-arch bug (`PORTING_PLAN.md` finding 41): the
`RTS_PREEMPTED` flag that `thread_yield` sets was never cleared, because the wasm platform layer
was missing the step every hardware arch runs at syscall return. A process that yielded and then
blocked in `RECEIVE` was woken but never re-linked, so the run queue drained and the console went
silent after the first command. The pre-queued input the check harness uses never reaches that
path, which is why nothing had seen it.

## What this does not do

- **No CDEV/termcap niceties.** The renderer handles `\n`, `\r`, `\b` and printable characters.
  That is all the shell's editor draws with. It is not a terminal emulator: no wrapping, no
  scrolling region, no ANSI escapes, no alternate screen. A real one is M5's `wserver`/canvas work.
- **No worker.** The guest runs on the main thread, so a slice is bounded by `SLICE_SYSCALLS` and
  the loop yields to the browser between slices. Moving the engine into a Worker would decouple the
  two, and would need the console to cross a `postMessage` boundary.
- **No persistence.** The boot filesystem is the image the host writes into the RAM disk's memory
  at every load, so anything written to it is gone on reload. That is M4 — a real block device
  behind the same BDEV protocol, backed by IndexedDB.
- **Not the only engine.** `tools/wasm-servers/boot.cjs` drives the same system to assert 63 facts
  about it and has its own copy of the mechanism, because a check harness needs no yielding. The
  two share a design rather than a file; `host.js`'s header says which parts are shared knowledge
  and where the authority is (`tools/fork-spike/` for the fork invariants).

## Verifying it

```sh
node tools/wasm-browser/run.js        # the engine: boot, a typed command, exit
node tools/wasm-browser/page.test.js  # the page's own code, under a stub DOM
sh tools/wasm-servers/run.sh           # the check harness, for the same artifacts
```

The first two are what a script can check. What they cannot: whether the browser paints it, whether
the real `fetch` and event loop behave, and whether a keystroke feels immediate. That part needs a
human and a tab.
