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
import { createTerminal } from './terminal.js';

const ARTIFACTS = {
  kernel: 'build/kernel.wasm',
  servers: 'build/servers.async.wasm',
  image: 'build/minixfs.img',
};

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
  status: document.getElementById('status'),
  panel: document.getElementById('panel'),
};

let host = null;
let stopping = false;
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

function paint() {
  frameQueued = false;
  elements.screen.textContent = `${terminal.lines.join('\n')}\n${terminal.partial}`;
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
      await waitForInput();
      continue;
    }

    // A slice. The case that matters is a slice that did nothing but retry the console read with
    // nothing to read: the guest is waiting for the keyboard, so stop pumping it and let the tab
    // idle. This is what keeps an idle prompt off the CPU.
    if (host.sliceWasSpinOnly() && host.console.pending === 0) {
      setStatus(describeState('awaiting-input'));
      await waitForInput(PARK_MS);
      continue;
    }

    setStatus(`running — ${host.steps} dispatch steps, ${host.procs.length} processes`);
    await nextFrame();
  }
}

function showDiagnostics(reason) {
  const notes = hostNotes.length > 0 ? hostNotes.join('\n') : '(none reported)';
  elements.panel.hidden = false;
  elements.panel.textContent =
    `${describeState(reason)}\n\nhost reports:\n${notes}\n\n` +
    `dispatch steps: ${host.steps}\n` +
    `processes: ${host.procs.map((p) => `${p.spec.label}(${p.spec.slot})`).join(' ')}\n` +
    `forks: ${host.forks.length}\n` +
    `syscalls of budget left: ${host.budget.left}`;
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

// ------------------------------------------------------------------------- bring-up

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
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
    host = createHost({
      kernel,
      servers,
      image,
      sink: {
        write: (byte) => {
          terminal.write(byte);
          repaint();
        },
        note: (name, detail) =>
          hostNotes.push(`${name}${detail === undefined ? '' : `: ${detail}`}`),
      },
    });
  } catch (error) {
    setStatus(`cannot start: ${error.message}`, 'bad');
    elements.panel.hidden = false;
    elements.panel.textContent =
      'The artifacts are missing, so build them first:\n\n' +
      '  sh tools/wasm-browser/build.sh\n\n' +
      `then reload this page. (${error.message})`;
    return;
  }
  await run();
}

main();
