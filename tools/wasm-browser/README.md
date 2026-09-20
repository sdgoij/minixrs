# The system in a browser tab

MINIX/Rust running in a page: the kernel, the servers and the shell, as WebAssembly, driven from
JavaScript by an engine that stops the guest between syscalls so the tab stays interactive — with a
disk behind it, so what you do in the tab is still there when you come back, and a canvas the
guest's own `fb` driver draws on.

```sh
sh tools/wasm-browser/build.sh     # build and stage the artifacts
node tools/wasm-browser/serve.js  # then open http://127.0.0.1:8080/
```

The page fetches its own copies of the artifacts (`build/`, which the first line stages), so a page
reloaded after a change to the guest — without re-running that line — is running the previous build.
One host per `host.js` is one spec list, so the mismatch shows up as a spec whose entry the module
does not export; that fails the boot naming the entry, the artifact sizes it loaded and this
command, rather than trapping inside a slot (`page.test.js` checks both, because both have happened).
The page fetches with `no-store`, so a reload after staging picks up what is on disk.

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
the harness does — DS, RS, PM, the RAM disk, VM, MFS, virtio-blk, devman, VFS, the tty and the
framebuffer server — INIT
execs `/bin/sh` out of the image, and the shell forks and execs for a command it cannot answer
itself.

On the page the block device is real: `virtio_blk` finds a device (host.js's `store`, the section
below), MFS mounts the root from it, and the shell's `>` redirect writes through the filesystem to
it. On the harness's diskless configuration the same driver finds nothing and MFS mounts the root
from the RAM disk instead, which is what a machine with an empty drive does.

The specs the page boots are `SYSTEM_SPECS` in `host.js`. The harness starts four more instances
it does not share: two DS clients and two program probes. Those are test apparatus, and they are
the only difference between what the checks boot and what the page boots.

## The files

| File | What it is |
|---|---|
| `index.html` | the page: a canvas for the guest's display, a console, the pane toggle, a status line, where the disk is, the two controls, and a panel for host reports |
| `page.js` | the DOM front end — the keyboard, the renderer, the panes, the disk, the display, the controls, and the pump loop |
| `host.js` | the engine: instances, the copy seam, exec, fork, the two devices, and the dispatch loop |
| `terminal.js` | the renderer's model: bytes in, lines out, with a cursor |
| `display.js` | the display's contract as the page implements it: the guest's frames onto the canvas |
| `store.js` | the store contract, and the page's implementation of it over IndexedDB |
| `file-store.js` | the same contract over a file, for the Node front ends |
| `run.js` | drives the engine from Node with scripted keystrokes (13 checks) |
| `page.test.js` | the server's MIME types, then `page.js` under a stub DOM — once with the disk, the display, the panes and their controls, once as a second tab that cannot have it (41 checks) |
| `indexeddb.fake.js` | a stub IndexedDB, so `page.test.js` can run the real store |
| `serve.js` | a static server, for `file://`'s sake |
| `build.sh` | stages what the page fetches, via `tools/wasm-servers/build.sh` |

## The disk

The guest's block device keeps its bytes in a *store*, which is the four calls a disk has rather
than the ones a database has (`store.js` documents the contract):

```js
{ imageId, setImageId(id), read(offset, length) -> bytes, write(offset, bytes) }
```

Both halves are synchronous, because `virtio_blk` gets a value back from `host_block_read`/
`host_block_write` and cannot be made to wait. IndexedDB is asynchronous, so the page's disk is held
in memory for the session: the store seeds it from the boot image, overlays every page the database
already holds, and answers reads from that array. Writes go the other way — into the array and on
into IndexedDB as one record per page, before the call returns.

Three consequences worth knowing before you use it:

- **One tab at a time.** The page holds the disk with a Web Lock while it is open, so a second tab
  is told the disk is taken rather than handed the same records to write — two in-memory copies over
  one database is the mixture `imageId` cannot see. That tab boots from the ramdisk and says so; a
  browser without the Web Locks API opens the disk unlocked.
- **`exit` is what makes a session durable.** MFS keeps its dirty blocks in its own cache until
  something flushes them, so a tab closed at the prompt leaves the disk *unclean* — the next mount
  reads it fine and refuses to write (`MFSFLAG_CLEAN`, `PORTING_PLAN.md` finding 48). Ending the
  session — `exit`, or the control that sends it for you — runs the guest's shutdown, which syncs,
  unmounts and marks the disk clean.
- **A write is durable a tick after it returns,** when the browser commits the transaction, rather
  than when the guest's `block_write` returns. A page killed in that window loses the blocks it was
  writing — the same promise a disk with a write cache makes. Finding 54 has the detail.

The store also refuses to be used with an image it was not made from: a rebuilt image over an old
disk would be two filesystems mixed, so the device is not attached at all and the page says why.
That is the state "start over from the boot image" exists for — it is the only way out of it from
the page.

### The two controls

Both are the page's answer to what the disk cannot promise by itself (finding 54), and both are
the same machinery the keyboard uses: bytes into the console.

- **end the session** sends `^U` and `exit`, which is what ends a session cleanly at all: this
  port's init *becomes* the shell by exec, so the guest's shutdown is reached by INIT exiting, and
  PM follows it with VFS's `pm_reboot` (sync, unmount, clean). The `^U` is why a half-typed line is
  not made into `echo hiexit`. A program in the foreground takes those bytes as its input, so a
  session with one running ends when that program does — the console is the only channel a host
  has, and there is no signal path to interrupt anything from here.
- **start over from the boot image** closes the store's own connection (a database with one open
  blocks its own deletion) and deletes it, then reloads: the next boot seeds the store from the
  image again. It is offered only to the tab that holds the disk — a tab that was refused the lock
  deleting the other one's disk is the mixture the lock exists to prevent.

Once the session has ended the page says so on the disk line, because that is the moment the tab is
safe to close: until then, MFS's dirty blocks are still the shutdown's business.

## The display

The canvas above the terminal is the *guest's* own display, not a decorator: the `fb` server boots
into a backend whose surface is its own memory and whose mode is the canvas's (`ARCH_WASM32.md`
§9.2, M5a). Two calls cross into the host — the mode, and one frame per flush — so what the canvas
shows is what the guest's driver put in its framebuffer:

```js
{ width, height, present(bytes) }   // `display.js`: a canvas; a recorder in the harnesses
```

`bytes` is one frame in the layout the driver describes — XRGB8888, four bytes per pixel, rows back
to back — which in memory is B,G,R,X, so `display.js` converts to the R,G,B,A an `ImageData` wants.
A frame that is not the mode's size is refused by `host.js` rather than shown: the two sides
disagreeing about the mode is a bug, and a partial picture looks like a drawing error.

With no window system in the guest yet, what is on the canvas is the `fb` driver's own verification
pattern — three bands, the same one the hardware arches get checked with — because the point of M5a
is the path, not the picture. `/dev/fb` is a real device a guest program can open and write to.

The canvas and the console are **two panes of one session**, and the toggle in the header decides
which you are looking at. Both stay live while hidden: the guest keeps drawing frames nobody is
looking at, and a terminal you switched away from keeps its line and its scroll position. The
keyboard goes to the guest either way — there is one console, and which pane shows it is your
choice rather than the guest's.

What the display does *not* do yet, and which piece of M5 each is:

- **The console is not drawn on it.** The terminal is still the host's renderer, byte by byte;
  putting the console on the canvas, cells and all, is M5b (`wserver`).
- **No mmap.** `/dev/fb` refuses `CDEV_MAP`: there are no page tables on this port to map a physical
  range through, and the surface is the server's own memory, which another instance cannot be given
  a view of. A client writes through the driver instead — which is what the reference's clients do.
- **No input.** Pointer and keyboard events from the page are not yet delivered to the guest; the
  wake they need does not exist on this port (M5c).

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

- **No CDEV/termcap niceties.** The renderer handles `\n`, `\r`, `\b` and printable characters,
  and the canvas beside it is the guest's own display (M5a). It does draw a cursor, and in the right
  place: the model has always tracked the column the shell editor moves with `\b` and `\r`, and the
  page draws a block there (blinking in CSS, so an idle tab still costs nothing). What is still
  missing is the console *on* the canvas: wrapping, scrolling regions, ANSI escapes and cursor
  addressing are M5b's `wserver`/cells work, not this renderer's.
- **No worker.** The guest runs on the main thread, so a slice is bounded by `SLICE_SYSCALLS` and
  the loop yields to the browser between slices. Moving the engine into a Worker would decouple the
  two, and would need the console to cross a `postMessage` boundary.
- **Not the only engine.** `tools/wasm-servers/boot.cjs` drives the same system to assert 68 facts
  about it and has its own copy of the mechanism, because a check harness needs no yielding. The
  two share a design rather than a file; `host.js`'s header says which parts are shared knowledge
  and where the authority is (`tools/fork-spike/` for the fork invariants).

## Verifying it

```sh
node tools/wasm-browser/run.js        # the engine: four boots over one disk
node tools/wasm-browser/page.test.js  # the page's own code, under a stub DOM
sh tools/wasm-servers/run.sh           # the check harness, for the same artifacts
```

The first two are what a script can check. What they cannot: whether the browser paints it, whether
the real `fetch`, event loop and IndexedDB behave, and whether a keystroke feels immediate. That
part needs a human and a tab.
