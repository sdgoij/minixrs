// The browser front end: the wasm system in a tab, with a terminal you can type at.
//
// This is the third front end over the same engine (`host.js`) and the same artifacts the check
// harness runs — the difference between them is entirely about *time*. A script can let the guest
// run to quiescence; a browser cannot, because a keystroke has to get in while the guest is
// running and an idle prompt must not pin the tab. That is what the pump loop below is for, and
// what `host.pump`'s slice is for.
//
// Two things are worth knowing before reading it:
//
//   * The guest's console read does not block on this port. The shell retries `read(0)` in user
//     mode and the tty does the same, so an idle console is a *spin* (`PORTING_PLAN.md` finding
//     32). That is why the loop cannot simply "run the guest and wait for a keystroke": it has to
//     stop the guest, and `sliceWasSpinOnly()` is how it knows the guest has nothing to do but
//     wait. Without that the tab would sit at 100% of a core while the user reads the screen.
//   * Stopping an instance is an Asyncify unwind, the same mechanism a blocked syscall uses: the
//     guest serialises itself and the host gets control back. It is not a pause button pressed
//     from outside — the engine stops *between* syscalls.

import { createHost } from './host.js';
import { createDisplay } from './display.js';
import { indexedDbStore } from './store.js';
import { createGateway, createWebSocketLink } from './net.js';
import { createTerminal } from './terminal.js';

const ARTIFACTS = {
  kernel: 'build/kernel.wasm',
  servers: 'build/servers.async.wasm',
  image: 'build/minixfs.img',
};

/// The database the disk lives in. Named here rather than in `store.js` because the lock
/// below is about this page's disk, not about stores in general.
const DISK_NAME = 'minixrs-disk';

/// How many syscalls one slice may run. A syscall here costs a few microseconds, so this is a few
/// milliseconds of guest work — short enough that a keystroke is picked up promptly, long enough
/// that a slice is work rather than overhead.
const SLICE_SYSCALLS = 400;

/// How long to leave a guest that is waiting for input alone before poking it again.
///
/// While the console queue is empty the guest is parked entirely — no slices at all — and a
/// keystroke wakes it immediately, so this is not a polling interval for input. It is a safety
/// valve: if the "it is only retrying the console read" reading is ever wrong, the tab is at most
/// this far from resuming by itself rather than waiting for a key that never comes. The cost when
/// idle is one slice per interval.
const PARK_MS = 2000;

const terminal = createTerminal({ maxLines: 800 });

const elements = {
  screen: document.getElementById('screen'),
  cursor: document.getElementById('cursor'),
  status: document.getElementById('status'),
  disk: document.getElementById('disk'),
  panel: document.getElementById('panel'),
  shutdown: document.getElementById('shutdown'),
  clear: document.getElementById('clear'),
  display: document.getElementById('display'),
  viewTerminal: document.getElementById('view-terminal'),
  viewDisplay: document.getElementById('view-display'),
};

/// The guest's display: the canvas, whose size *is* the mode the guest's `fb` driver adopts (M5).
/// Built at the top level rather than at bring-up because it is part of what the engine is given:
/// a page with no canvas is a page whose guest has no display, which is the headless case.
const display = createDisplay(elements.display);

/// The network link this page attaches (M6), and what to call it.
///
/// The in-page gateway unless the page was opened with `?net=<websocket url>`, in which case the
/// frames are tunnelled to a relay (`tools/wasm-net/relay.js`) instead. A query parameter rather
/// than a build-time choice because both wires serve the *same* guest — what changes is where the
/// frames go — and a demo that had to be rebuilt to show the other one would be two demos. The
/// value is deliberately a whole WebSocket URL: the page has no business deciding which relay is
/// the right one, and a reader who runs one knows its address.
function openLink() {
  const wanted = new URLSearchParams(window.location?.search ?? '').get('net');
  if (wanted === null || wanted === '') {
    return { link: createGateway(), name: 'net: the in-page gateway' };
  }
  return { link: createWebSocketLink({ url: wanted }), name: `net: relay ${wanted}` };
}

const net = openLink();

let host = null;
let stopping = false;
/// The store this tab's disk lives in, or null when it has none: the page offers to start over
/// on it, which is the only way out of a store the device refuses.
let disk = null;
/// Whether this tab has asked the guest to end the session (`endSession`).
let shutdownAsked = false;
/// Resolves the current wait-for-input, if the loop is parked.
let wake = null;
/// The host's own reports (`note`): how a dropped fork or an unloadable module reaches the
/// reader, since a host-side failure has no other way onto the page.
const hostNotes = [];

// ------------------------------------------------------------------------- rendering
//
// Coalesced to one repaint per frame: the guest writes a byte at a time, and a repaint per byte
// would spend more time in the DOM than in the guest.

let frameQueued = false;

/// Draw the console.
///
/// The screen is the committed lines and then the line being typed, split at the renderer's cursor
/// column: everything before it in the `pre`'s own text, everything after it in the cursor element,
/// and the cursor block itself is the CSS between them (`index.html`). Splitting there rather than
/// appending a block is what puts the cursor where the *shell* has it — the editor moves it with
/// `\b` and `\r` — and not always at the end of the line.
function paint() {
  frameQueued = false;
  const partial = terminal.partial;
  const col = terminal.col;
  elements.screen.textContent = `${terminal.lines.join('\n')}\n${partial.slice(0, col)}`;
  elements.cursor.textContent = partial.slice(col);
  elements.screen.append(elements.cursor);
  elements.screen.scrollTop = elements.screen.scrollHeight;
}

function repaint() {
  if (frameQueued) return;
  frameQueued = true;
  requestAnimationFrame(paint);
}

function setStatus(text, kind = '') {
  elements.status.textContent = text;
  elements.status.dataset.kind = kind;
}

/// Where the disk is, or why there is none. Set once, at bring-up: a page whose writes go
/// to IndexedDB and a page booting from the ramdisk look identical from the terminal, and
/// the difference is whether anything you do survives a reload.
function setDisk(text, kind = '') {
  if (elements.disk === null) return;
  elements.disk.textContent = text;
  elements.disk.dataset.kind = kind;
}

// --------------------------------------------------------------------------- the pump

const nextFrame = () => new Promise((resolve) => requestAnimationFrame(resolve));

/// Wait until there is something for the guest to read.
///
/// A keystroke resolves this at once. `ms` bounds the wait so a guest parked on a wrong reading
/// resumes by itself; with `ms` omitted the wait is bounded only by input, which is right when
/// the guest is genuinely idle and nothing else can happen.
function waitForInput(ms) {
  if (host.console.pending > 0) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = ms === undefined ? null : setTimeout(finish, ms);
    function finish() {
      if (timer !== null) clearTimeout(timer);
      wake = null;
      resolve();
    }
    wake = finish;
  });
}

function describeState(reason) {
  switch (reason) {
    case 'awaiting-input':
      return 'waiting for input (the guest retries its console read in user mode — finding 32)';
    case 'quiescent':
      return 'idle — nothing runnable';
    case 'budget':
      return 'stopped: the syscall budget ran out, so a guest is spinning';
    case 'trapped':
      return 'stopped: an instance faulted — its report is below';
    case 'steps':
      return 'stopped: too many dispatch steps in one pump';
    default:
      return `stopped: ${reason}`;
  }
}

async function run() {
  for (;;) {
    if (stopping) return;

    const reason = host.pump({ maxSyscalls: SLICE_SYSCALLS });
    repaint();

    if (host.halted !== null) {
      setStatus(`halted by the guest (code ${host.halted})`, 'bad');
      return;
    }
    if (reason !== 'slice' && reason !== 'quiescent') {
      setStatus(describeState(reason), 'bad');
      showDiagnostics(reason);
      return;
    }
    if (reason === 'quiescent') {
      setStatus(describeState(reason));
      if (shutdownAsked) sessionEnded();
      await waitForInput();
      continue;
    }

    // A slice. The case that matters is a slice that did nothing but retry the console read with
    // nothing to read: the guest is waiting for the keyboard, so stop pumping it and let the tab
    // idle. This is what keeps an idle prompt off the CPU.
    //
    // `input.pending` is the other half of "idle": a pointer record the guest has been *told*
    // about is work, and the instance that wake made runnable sits behind the spinning shell in the
    // run queue — this port rotates a process only when a syscall ends, and the shell's read never
    // ends the dispatch. Parking there would drop the event until the next key.
    if (
      host.sliceWasSpinOnly() &&
      host.console.pending === 0 &&
      host.input.pending === 0
    ) {
      setStatus(describeState('awaiting-input'));
      await waitForInput(PARK_MS);
      continue;
    }

    setStatus(`running — ${host.steps} dispatch steps, ${host.procs.length} processes, ${net.name}`);
    await nextFrame();
  }
}

function showPanel(text) {
  elements.panel.hidden = false;
  elements.panel.textContent = text;
}

function reportsText() {
  return hostNotes.length > 0 ? hostNotes.join('\n') : '(none reported)';
}

function showDiagnostics(reason) {
  showPanel(
    `${describeState(reason)}\n\nhost reports:\n${reportsText()}\n\n` +
      `dispatch steps: ${host.steps}\n` +
      `processes: ${host.procs.map((p) => `${p.spec.label}(${p.spec.slot})`).join(' ')}\n` +
      `forks: ${host.forks.length}\n` +
      `syscalls of budget left: ${host.budget.left}`
  );
}

// ----------------------------------------------------------------------- the views

/// Show one pane: the console, or the guest's display. Both stay live while hidden — the guest
/// draws whether or not anyone is looking, and a terminal that was scrolled away comes back where
/// it was — so this is only about what the reader sees.
function showView(which) {
  const showingDisplay = which === 'display';
  elements.screen.hidden = showingDisplay;
  elements.display.hidden = !showingDisplay;
  for (const [button, active] of [
    [elements.viewTerminal, !showingDisplay],
    [elements.viewDisplay, showingDisplay],
  ]) {
    button.dataset.active = String(active);
    button.setAttribute('aria-pressed', String(active));
  }
}

function wireViews() {
  elements.viewTerminal.addEventListener('click', () => showView('terminal'));
  elements.viewDisplay.addEventListener('click', () => showView('display'));
}

wireViews();
// The pane the page opens on, set here rather than left to `index.html`'s attributes: the two agree
// (the HTML is what a reader sees before this module runs), and what a check reads is this.
showView('terminal');

// ---------------------------------------------------------------------- the controls
//
// Both exist because of what the disk cannot promise by itself (`PORTING_PLAN.md` finding 54).
// A session only ends cleanly if something asks it to — MFS holds its dirty blocks until a
// shutdown syncs them, so a tab closed at the prompt leaves the filesystem unclean and the next
// boot mounts it read-only — and a store the page refuses is otherwise a page with no way back,
// since the device is never attached and nothing at the prompt can clear what refuses it.

/// End the session. The shutdown is the guest's, and the guest gets it from INIT's exit: this
/// port's init *becomes* the shell by exec, so `exit` at the prompt is the whole mechanism.
/// There is no other lever — a host cannot ask a guest to stop, and the console is the only
/// channel it has.
///
/// ^U first, so a half-typed line does not become the command. A program in the foreground takes
/// the bytes as its own input, so the session ends when that program does.
function endSession() {
  if (shutdownAsked) return;
  shutdownAsked = true;
  elements.shutdown.disabled = true;
  setStatus('ending the session — the guest is shutting down…');
  send('\x15exit\n');
}

/// The session is over, and the disk is the shutdown's: it was synced, unmounted and marked
/// clean, which is the moment this tab can be closed without the next boot paying for it.
/// Setting the same two lines again is harmless, so this does not need a flag of its own.
function sessionEnded() {
  setDisk('disk: IndexedDB — the session ended, so the filesystem is clean');
  setStatus('the session ended — the disk is clean and this tab can be closed');
}

/// Throw the disk away and start from the boot image again. This is the way out of the two
/// states that have no other one: a store whose contents came from a different image, which the
/// device refuses to attach, and a filesystem a previous tab left unclean, which the next mount
/// reads but will not write.
async function startOver() {
  elements.clear.disabled = true;
  setStatus('clearing the disk…');
  // The guest's disk is about to go away underneath it and this page is on its way to a reload,
  // so the pump stops first: nothing should be writing to a store that is being deleted.
  stopping = true;
  try {
    await disk.clear();
  } catch (error) {
    hostNotes.push(`the disk could not be cleared: ${error}`);
    setStatus(`the disk could not be cleared: ${error.message}`, 'bad');
    showPanel(`the disk could not be cleared.\n\n${reportsText()}`);
    return;
  }
  location.reload();
}

function wireControls() {
  elements.shutdown.addEventListener('click', endSession);
  if (disk === null) {
    // Nothing to clear: this browser has no IndexedDB, or another tab holds the disk — and in
    // that case deleting it from here would be deleting another session's disk, which is the
    // mixture the lock exists to prevent. The disk line and the panel say which it was.
    elements.clear.disabled = true;
    return;
  }
  elements.clear.addEventListener('click', startOver);
}

// ---------------------------------------------------------------------- the keyboard

/// What a key sends the guest, for the keys that are not a character.
///
/// The shell's editor understands all of these: the arrows walk its history and cursor, Home and
/// End jump, and `\x7f` is its backspace. Ctrl+letter is handled generically, which is what makes
/// ^C, ^D, ^U, ^W, ^K and ^L work without listing them.
const KEY_BYTES = {
  Enter: '\n',
  Backspace: '\x7f',
  Delete: '\x7f',
  Tab: '\t',
  Escape: '\x1b',
  ArrowUp: '\x1b[A',
  ArrowDown: '\x1b[B',
  ArrowRight: '\x1b[C',
  ArrowLeft: '\x1b[D',
  Home: '\x01',
  End: '\x05',
};

function send(bytes) {
  host.console.push(bytes);
  if (wake !== null) wake();
}

window.addEventListener('keydown', (event) => {
  if (host === null) return;
  if (event.metaKey || event.altKey) return;
  if (event.ctrlKey) {
    if (event.key.length === 1) {
      const code = event.key.toUpperCase().charCodeAt(0) - 64;
      if (code >= 1 && code <= 26) {
        event.preventDefault();
        send(String.fromCharCode(code));
      }
    }
    return;
  }
  const mapped = KEY_BYTES[event.key];
  if (mapped !== undefined) {
    event.preventDefault();
    send(mapped);
    return;
  }
  if (event.key.length === 1) {
    event.preventDefault();
    send(event.key);
  }
});

window.addEventListener('paste', (event) => {
  if (host === null) return;
  const text = event.clipboardData?.getData('text');
  if (text !== undefined && text !== '') {
    event.preventDefault();
    send(text);
  }
});

// The pointer is the one input the *desktop* owns on this port (M5c). Keys stay on the console: the
// page's keyboard is this port's UART, and a key has a consumer already (`page.js`'s own mapping maps
// it to the byte the shell's line editor wants). A pointer has none — nothing about the console takes
// one — so a pointer event on the canvas becomes an HID record and the guest's input server is what
// routes it, which is what moves the arrow the compositor draws over its desktop.
//
// The records are *absolute*: a canvas says where the pointer is, and the guest's ABS page is the one
// that carries a position — normalized to 0..0x7FFF, the way a virtio tablet reports one, which the
// desktop scales to its own pixels.
const BUTTON_PAGE = 0x0009;
const ABS_PAGE = 0x00fd;
const ABS_X = 0x0030;
const ABS_Y = 0x0031;
/// The HID usage each browser `event.button` maps to: left is 1, right is 2, middle is 3, which is
/// the order the records carry them and not the order the DOM numbers them.
const BUTTON_USAGE = { 0: 1, 1: 3, 2: 2 };
/// The most a normalized coordinate may be: one less than the divisor the desktop uses.
const ABS_MAX = 32767;

/// Send one input record to the guest, and wake a parked pump if there is one.
///
/// The wake matters for the reason the console's does: the guest parked because the only instance
/// the dispatcher reaches is the shell retrying its read, and a woken input server needs a slice
/// before it can drain anything.
function sendInput(page, code, value) {
  host.input.push(page, code, value);
  if (wake !== null) wake();
}

/// Where the pointer is on the canvas, normalized to the range the guest's ABS page uses.
///
/// The canvas is the guest's *mode*, so the fraction across it is what the guest needs; the CSS box
/// may be a different size, which is why this reads the rectangle rather than the attributes.
function normalise(event) {
  const rect = elements.display.getBoundingClientRect();
  const across = (event.clientX - rect.left) / Math.max(rect.width, 1);
  const down = (event.clientY - rect.top) / Math.max(rect.height, 1);
  return [
    Math.min(Math.max(Math.round(across * ABS_MAX), 0), ABS_MAX),
    Math.min(Math.max(Math.round(down * ABS_MAX), 0), ABS_MAX),
  ];
}

function sendPointer(event) {
  if (host === null) return;
  const [x, y] = normalise(event);
  sendInput(ABS_PAGE, ABS_X, x);
  sendInput(ABS_PAGE, ABS_Y, y);
}

/// Which buttons the page is holding down, so a pointer that leaves the canvas can release them.
///
/// A desktop that never hears the release keeps dragging, and the drag ends when the button is let
/// go — which on this side of the boundary is this map and not the guest's.
const heldButtons = new Set();

function sendButton(event, pressed) {
  if (host === null) return;
  const usage = BUTTON_USAGE[event.button];
  if (usage === undefined) return;
  if (pressed) {
    heldButtons.add(usage);
  } else {
    heldButtons.delete(usage);
  }
  sendInput(BUTTON_PAGE, usage, pressed ? 1 : 0);
}

elements.display.addEventListener('pointermove', sendPointer);
elements.display.addEventListener('pointerdown', (event) => {
  event.preventDefault();
  // The position first: a click that arrives before the move would land on the old one.
  sendPointer(event);
  sendButton(event, true);
});
elements.display.addEventListener('pointerup', (event) => sendButton(event, false));
elements.display.addEventListener('pointerleave', () => {
  for (const usage of heldButtons) sendInput(BUTTON_PAGE, usage, 0);
  heldButtons.clear();
});

// ------------------------------------------------------------------------- bring-up

async function fetchBytes(url) {
  // `no-store` on purpose: these are rebuilt in place under the page's feet by
  // `tools/wasm-browser/build.sh`, and a cached module is a boot that claims an artifact is
  // stale when the file on disk is not (`serve.js` sends the same header, for servers that are
  // not this one).
  const response = await fetch(url, { cache: 'no-store' });
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

/// Hold the page's disk for this tab, so a second one cannot write it.
///
/// Two tabs are two in-memory disks over one set of records — a superblock from one session
/// left over the blocks of another — which is the mixture the store's `imageId` guard exists
/// to prevent, arriving by another route. The Web Locks API is what makes this cheap: a lock
/// is held while the callback's promise is pending, and the browser drops it when the
/// document goes away, so a closed tab does not leave the disk locked. A browser without the
/// API opens the disk unlocked, which is what this page did before.
///
async function takeDisk(name) {
  const locks = globalThis.navigator?.locks;
  if (locks === undefined) return true;
  // Never settles: the lock is held for as long as this document exists.
  const held = new Promise(() => {});
  return new Promise((resolve) => {
    locks
      .request(`${name}-owner`, { ifAvailable: true }, (lock) => {
        resolve(lock !== null);
        return lock === null ? undefined : held;
      })
      .catch((error) => {
        hostNotes.push(`the disk is not locked to this tab: ${error}`);
        resolve(true);
      });
  });
}

/// The disk, or null when this page cannot have one.
///
/// The boot image is the disk's initial contents and IndexedDB holds what the guest
/// writes, so a reload comes back to the filesystem the last session left — which is what
/// the guest's own shutdown makes worth having. There are three ways to end up without a
/// disk, and all of them are reported rather than thrown away: another tab holds it, this
/// browser has no IndexedDB (a private window, say), or the disk's contents came from
/// another image (`host.js` refuses that one).
async function openDisk(image) {
  if (!(await takeDisk(DISK_NAME))) {
    hostNotes.push('another tab has the disk open, so this page booted from the ramdisk');
    return null;
  }
  try {
    return await indexedDbStore({
      name: DISK_NAME,
      imageBytes: image,
      // A write that cannot reach the database has already been reported to the guest as
      // done, so this is the only place it can be seen — and it belongs on the page, not
      // only in a report a reader has to trigger.
      onError: (error) => {
        hostNotes.push(`the disk could not be written: ${error}`);
        setDisk('disk: writes are failing, and this session is only in memory', 'bad');
      },
    });
  } catch (error) {
    hostNotes.push(`no disk: ${error.message}`);
    return null;
  }
}

async function main() {
  // Read by `index.html`'s loader, which reports a module that never ran as a load failure: a MIME
  // type the browser refuses leaves nothing else to go on.
  window.minixrsStarted = true;
  paint();
  setStatus('loading the kernel, the servers and the boot image…');
  try {
    const [kernel, servers, image] = await Promise.all([
      fetchBytes(ARTIFACTS.kernel),
      fetchBytes(ARTIFACTS.servers),
      fetchBytes(ARTIFACTS.image),
    ]);
    disk = await openDisk(image);
    host = createHost({
      kernel,
      servers,
      image,
      store: disk,
      display,
      net: net.link,
      sink: {
        write: (byte) => {
          terminal.write(byte);
          repaint();
        },
        note: (name, detail) =>
          hostNotes.push(`${name}${detail === undefined ? '' : `: ${detail}`}`),
      },
    });
    if (host.device.attached) {
      setDisk('disk: IndexedDB — the root came off it and writes go back to it');
    } else {
      // Whatever the reason, it is in the reports and a reader cannot guess it: a store
      // that was opened and then refused (its contents came from another image) leaves this
      // boot on the ramdisk just as surely as having no disk at all.
      setDisk('disk: none — this boot is the ramdisk', 'bad');
      showPanel(`this boot has no disk.\n\n${reportsText()}`);
    }
    wireControls();
  } catch (error) {
    setStatus(`cannot start: ${error.message}`, 'bad');
    showPanel(
      'The artifacts are missing, so build them first:\n\n' +
        '  sh tools/wasm-browser/build.sh\n\n' +
        `then reload this page. (${error.message})`
    );
    return;
  }
  await run();
}

main();
