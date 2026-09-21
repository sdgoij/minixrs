// Run the page's own code under Node, with a DOM stub.
//
//   node tools/wasm-browser/page.test.js
//
// Two things are checked here. First the **server**, because a module script is fetched under
// strict MIME checking and a static server that answers `text/plain` for a JavaScript file leaves
// a page that will not start with nothing on it to say why — the browser reports it only in its
// own console, which is a round trip through a human. Second the **page**, under a stub DOM:
// `run.js` covers the engine and the renderer but never touches `page.js`, which is where the DOM
// is and therefore the file a script cannot normally reach. Between the two, the real browser is
// left as the only untested part — pixels, event ordering, and whether a keystroke feels
// immediate — and every other line of `page.js` runs here.

import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { SYSTEM_SPECS, createHost } from './host.js';
import { fakeIndexedDB } from './indexeddb.fake.js';
import { indexedDbStore } from './store.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const buildDir = path.resolve(here, '../wasm-servers/build');
const imagePath = path.join(here, 'build/minixfs.img');

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail !== undefined && !ok) console.log(`        ${detail}`);
}

// ------------------------------------------------------------------- the server's MIME types

/// Start `serve.js` on a port the OS picks, and hand back where it is and how to stop it.
async function startServer() {
  const child = spawn(process.execPath, [path.join(here, 'serve.js')], {
    env: { ...process.env, PORT: '0' },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const port = await new Promise((resolve, reject) => {
    let out = '';
    child.stdout.on('data', (chunk) => {
      out += chunk;
      const match = /http:\/\/127\.0\.0\.1:(\d+)\//.exec(out);
      if (match) resolve(Number(match[1]));
    });
    child.on('error', reject);
    setTimeout(() => reject(new Error(`serve.js did not start:\n${out}`)), 10000);
  });
  return { child, port };
}

const server = await startServer();
const base = `http://127.0.0.1:${server.port}`;

try {
  const page = await fetch(`${base}/`);
  check(
    'the server serves index.html as HTML',
    page.ok && (page.headers.get('content-type') ?? '').includes('text/html'),
    `status=${page.status} type=${page.headers.get('content-type')}`
  );

  // The one that matters: a module script is refused before it runs unless this is a JavaScript
  // type, so a wrong answer here is a page that loads to a blank screen with a console message.
  for (const file of ['page.js', 'host.js', 'store.js', 'terminal.js']) {
    const response = await fetch(`${base}/${file}`);
    const type = response.headers.get('content-type') ?? '';
    check(
      `${file} is served as JavaScript, so the browser will accept it as a module`,
      response.ok && /(text|application)\/(java|ecma)script/.test(type),
      `status=${response.status} type=${type}`
    );
  }

  const wasm = await fetch(`${base}/build/kernel.wasm`);
  const wasmBytes = wasm.ok ? (await wasm.arrayBuffer()).byteLength : 0;
  check(
    'the kernel artifact is served, and is not empty',
    wasm.ok && wasmBytes > 0,
    `status=${wasm.status} bytes=${wasmBytes}`
  );
} finally {
  server.child.kill();
}

// ------------------------------------------------------------------------- the stub DOM

const elements = new Map();
class StubElement {
  constructor(id) {
    this.id = id;
    this.ownText = '';
    this.children = [];
    this.dataset = {};
    this.attributes = new Map();
    this.hidden = false;
    this.disabled = false;
    this.scrollTop = 0;
    this.scrollHeight = 0;
    this.byEvent = new Map();
  }
  /// The DOM's own semantics, which the page now depends on: `textContent` is this element's text
  /// plus its children's, and *setting* it drops the children. The console is a `pre` with the
  /// cursor as its last child, so a stub whose `textContent` ignored children would hide half of
  /// every line from a check.
  get textContent() {
    return this.ownText + this.children.map((child) => child.textContent).join('');
  }
  set textContent(value) {
    this.ownText = String(value);
    this.children = [];
  }
  append(child) {
    this.children.push(child);
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }
  addEventListener(type, fn) {
    if (!this.byEvent.has(type)) this.byEvent.set(type, []);
    this.byEvent.get(type).push(fn);
  }
  /// Press the control, the browser's way — a disabled one does not fire — and answer whether
  /// anything was listening, so a check can tell a live control from a dead one.
  click() {
    if (this.disabled) return false;
    for (const fn of this.byEvent.get('click') ?? []) fn({ preventDefault() {} });
    return true;
  }
}
globalThis.document = {
  getElementById(id) {
    if (!elements.has(id)) elements.set(id, new StubElement(id));
    return elements.get(id);
  },
};

/// The page's display is a canvas, which is the one element the stub DOM has to do more than hold
/// text for: `display.js` asks for a 2D context, allocates an `ImageData` and draws it. The stub
/// keeps what was drawn, so a check reads the pixels the *guest* put there — through the same
/// conversion the browser would do.
class StubCanvas extends StubElement {
  constructor(id, width, height) {
    super(id);
    this.width = width;
    this.height = height;
    this.frames = 0;
    this.image = null;
  }

  getContext() {
    return {
      createImageData: (width, height) => ({
        width,
        height,
        data: new Uint8ClampedArray(width * height * 4),
      }),
      putImageData: (image) => {
        this.image = image;
        this.frames += 1;
      },
    };
  }

  /// Where the canvas is on the page, which the page's pointer handling needs: it normalizes a
  /// `clientX`/`clientY` through this rectangle (M5c). A stub that could not answer would make every
  /// pointer event throw, and the size is the canvas's own, so a client position here *is* a desktop
  /// position.
  getBoundingClientRect() {
    return { left: 0, top: 0, width: this.width, height: this.height };
  }
}

elements.set('display', new StubCanvas('display', 1024, 768));

// The one thing a stub DOM cannot do for the page: starting over finishes by reloading, and this
// process has no `location`.
let reloads = 0;
Object.defineProperty(globalThis, 'location', {
  value: {
    reload: () => {
      reloads += 1;
    },
  },
  configurable: true,
});

const listeners = new Map();
globalThis.window = {
  addEventListener(type, fn) {
    if (!listeners.has(type)) listeners.set(type, []);
    listeners.get(type).push(fn);
  },
};

/// The frames the page asks for, run as timers: the page's loop awaits them, so they have to
/// actually happen for the loop to make progress.
globalThis.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);

// The lock the page takes on its disk. It holds one for as long as the tab is open, so two tabs
// cannot both keep an in-memory copy and write their own: the store's `imageId` catches a *rebuilt*
// image over an old disk, and the lock catches the same mixture arriving from two live sessions.
// Node has no `navigator`, so without a stub the page never runs that path at all.
//
// The fake is the browser's semantics and no more. A name is held by whoever asks for it first,
// the callback is answered on a later task, and the request settles with what the callback
// returns — which is what makes the page's never-settling `held` hold the lock rather than
// release it, and what a page that got `null` instead has to notice.
function fakeLocks() {
  const held = new Set();
  return {
    held,
    request(name, options, callback) {
      const free = options?.ifAvailable !== true || !held.has(name);
      if (free) held.add(name);
      return new Promise((resolve) => queueMicrotask(() => resolve(callback(free ? { name } : null))));
    },
  };
}

const locks = fakeLocks();
Object.defineProperty(globalThis, 'navigator', { value: { locks }, configurable: true });

// The page's disk. `page.js` opens a store over IndexedDB at bring-up, so the page cannot
// start under Node without one; the fake is the browser's own implementation replaced, not
// the page's code stubbed out, and everything the store does above it is exercised for
// real.
globalThis.indexedDB = fakeIndexedDB();

const ARTIFACT_BYTES = {
  'build/kernel.wasm': fs.readFileSync(path.join(buildDir, 'kernel.wasm')),
  'build/servers.async.wasm': fs.readFileSync(path.join(buildDir, 'servers.async.wasm')),
  'build/minixfs.img': fs.readFileSync(imagePath),
};
globalThis.fetch = async (url) => {
  const bytes = ARTIFACT_BYTES[url];
  if (bytes === undefined) return { ok: false, status: 404 };
  return { ok: true, status: 200, async arrayBuffer() { return bytes; } };
};

function press(key, modifiers = {}) {
  const event = {
    key,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    preventDefault() {},
    ...modifiers,
  };
  for (const fn of listeners.get('keydown') ?? []) fn(event);
  return event;
}

const screen = () => document.getElementById('screen').textContent;
const status = () => document.getElementById('status').textContent;
const diskLabel = () => document.getElementById('disk').textContent;

/// Poll until `predicate` holds, so the test waits on the page's own progress rather than on a
/// guess about how long a boot takes.
async function until(predicate, what, timeoutMs = 60000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  console.log(`        timed out waiting for ${what}`);
  console.log(`        status: ${status()}`);
  console.log(`        screen:\n${screen()}`);
  return false;
}

// -------------------------------------------------------------------------- the run

// The page starts itself on import, which is what a browser does too.
await import('./page.js');

check(
  'the page loads the artifacts and boots to a prompt',
  await until(() => screen().includes('# '), 'the shell prompt'),
  `status: ${status()}`
);
check(
  'the status line says the guest is waiting for input rather than running',
  await until(() => status().includes('waiting for input'), 'the parked status'),
  `status: ${status()}`
);

// The keyboard mapping, through the page's own handler: this is what a human's keystrokes take.
for (const ch of '/bin/echo hi') press(ch);
check('a printable key is sent as itself', await until(() => screen().includes('/bin/echo hi'), 'the echo'));
press('Backspace');
check(
  'backspace sends the byte the shell editor expects (0x7f, not 0x08)',
  await until(() => screen().includes('/bin/echo h') && !screen().includes('/bin/echo hi'), 'the erased char'),
  `screen: ${JSON.stringify(screen().slice(-40))}`
);
press('i');
press('Enter');
check(
  "the command runs: it forks, the child exec's /bin/echo, and its output reaches the page",
  await until(() => screen().split('\n').includes('hi'), 'the command output'),
  `screen:\n${screen()}`
);

press('ArrowUp');
check(
  'an arrow key sends the escape sequence the editor uses for history',
  await until(() => screen().includes('# /bin/echo hi'), 'the recalled line'),
  `screen: ${JSON.stringify(screen().slice(-40))}`
);

press('u', { ctrlKey: true });
check(
  '^U reaches the editor as a kill-line',
  await until(() => !screen().split('\n').slice(-1)[0].includes('/bin/echo hi'), 'the killed line'),
  `screen: ${JSON.stringify(screen().slice(-40))}`
);

// And the loop is *parked*, not spinning: a page that burned a core at the prompt would be the
// whole reason for the slice/park machinery, so the cheapest check for it is that nothing moves
// while nothing is typed. The settling wait comes first because a keystroke's redraw (the
// editor blanks the row and rewrites it) is still arriving when the previous check returns.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function untilStable(quietMs = 50, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  let last = screen();
  while (Date.now() < deadline) {
    await sleep(quietMs);
    if (screen() === last) return last;
    last = screen();
  }
  return last;
}

await untilStable();
const before = screen();
await sleep(300);
check(
  'the page idles at the prompt instead of spinning the guest',
  screen() === before && status().includes('waiting for input'),
  `screen grew: ${before.length} -> ${screen().length}; status: ${status()}`
);

// -------------------------------------------------------------------------- the display
//
// The guest's own display (M5): the fb server's surface, the compositor's desktop, and the canvas.
// What is checked here is the whole path in one place — the guest's pixels, presented through the
// host import, converted by `display.js` and read back out of the canvas — because each layer's
// claim is only worth something if the one above it agrees.
//
// The frame at the end of the run is the *compositor's* (M5b): the console's window on the desktop,
// which is what M5b is for. The driver's own verification pattern is the first frame `fb` presents
// at boot and belongs to M5a — `tools/wasm-servers/boot.cjs` checks it there, where the recorder can
// keep both frames.

const canvas = document.getElementById('display');
const pixelAt = (x, y) => {
  const data = canvas.image.data;
  const at = (y * canvas.width + x) * 4;
  return [data[at], data[at + 1], data[at + 2], data[at + 3]];
};

/// A white pixel anywhere in the console window's title strip: the 8x16 font drew the title there.
const titleIsRasterized = () => {
  for (let x = 196; x < 400; x += 1) {
    if (pixelAt(x, 192).join() === '255,255,255,255') return true;
  }
  return false;
};

check(
  "the guest's fb server reports the host's canvas as its backend",
  await until(() => screen().includes('fb: backend host canvas'), 'the fb boot line'),
  `screen:\n${screen()}`
);
check(
  'the canvas is the display: the guest drew a frame into it',
  await until(() => canvas.frames >= 1, 'the first frame'),
  `frames=${canvas.frames}`
);
// The compositor's window arrives a few frames later than the driver's boot pattern, and the draw
// happens on an animation frame (`display.js`), so this waits for the desktop rather than assuming
// it is already drawn.
const showingDesktop = await until(
  () => pixelAt(400, 190).join() === '64,128,192,255',
  "the console window's title bar on the canvas"
);
check(
  "the canvas shows the desktop the guest composed, in the channels a canvas wants",
  // Desktop background outside the console's window, the focused title bar's colour inside its
  // title, and the body's colour in its body. The title bar is 0x004080C0, so as canvas RGBA it is
  // 64,128,192 — which is also the check that `display.js` converts channel order rather than
  // handing the guest's B,G,R,X bytes over as they lie (they would read 192,128,64).
  showingDesktop &&
    pixelAt(20, 20).join() === '40,40,40,255' &&
    pixelAt(300, 300).join() === '24,24,32,255' &&
    titleIsRasterized(),
  `desktop=${pixelAt(20, 20)} title=${pixelAt(400, 190)} body=${pixelAt(300, 300)} frames=${canvas.frames}`
);

// A pointer event on the canvas is the browser's input reaching the guest, and the compositor is where
// it becomes visible: it draws the arrow over its own desktop. So this reads the whole path off one
// pixel — the page's DOM handler, the host's queue, the interrupt, the input server, the consumer, the
// desktop's overlay — and it is the only check of M5c that goes through the page rather than through
// the engine's API (`run.js` drives that half, including the control that the *wake* is what makes the
// guest look).
//
// The stub canvas's rectangle is its own size, so a client position is a desktop position: the page
// normalizes to the guest's 0..0x7FFF and the desktop scales back, and 300/400 is what comes out.
const POINTER_X = 300;
const POINTER_Y = 400;
const arrowAt = (x, y) => {
  for (let row = 0; row < 12; row += 1) {
    for (let col = 0; col < 8; col += 1) {
      if (pixelAt(x + col, y + row).join() === '255,255,255,255') return true;
    }
  }
  return false;
};

check(
  'the desktop has not drawn its arrow where this check will move it',
  !arrowAt(POINTER_X, POINTER_Y),
  `pixels at ${POINTER_X},${POINTER_Y}: ${pixelAt(POINTER_X, POINTER_Y)}`
);
for (const fn of canvas.byEvent.get('pointermove') ?? []) {
  fn({ clientX: POINTER_X, clientY: POINTER_Y, preventDefault() {} });
}
check(
  "a pointer event on the canvas moves the guest desktop's arrow",
  await until(() => arrowAt(POINTER_X, POINTER_Y), 'the pointer overlay'),
  `pixels at ${POINTER_X},${POINTER_Y}: ${pixelAt(POINTER_X, POINTER_Y)}; status: ${status()}`
);

// A drag is the same path many times over, and it is the shape a reader actually produces. It is also
// where the cost first mattered: a browser delivers a pointermove per frame, so a drag of a few
// hundred turns is a few hundred rounds of the input server and the desktop, and while a yield could
// not switch processes each of those rounds cost a whole slice of the tty's console-read retry
// (finding 62). A reader dragging over the desktop got `stopped: the syscall budget ran out`.
//
// The number of moves is the one the old cost could not afford: it measured ~1400 syscalls per
// record, so this is past the 1M budget and the page would have stopped with the arrow somewhere
// behind the pointer. What the check asserts is that the drag *arrives* — the arrow at the last
// position the events named — rather than where the guest ran out of budget.
const DRAG_MOVES = 800;
const DRAG_FROM = 120;
let dragLast = DRAG_FROM;
for (let i = 0; i < DRAG_MOVES; i += 1) {
  dragLast = DRAG_FROM + i;
  for (const fn of canvas.byEvent.get('pointermove') ?? []) {
    fn({ clientX: dragLast, clientY: POINTER_Y, preventDefault() {} });
  }
}
check(
  'a drag through the page arrives, rather than spending the budget on the turns in between',
  await until(() => arrowAt(dragLast, POINTER_Y), `the arrow at ${dragLast}`),
  `arrow at ${dragLast},${POINTER_Y} is ${pixelAt(dragLast, POINTER_Y)}; status: ${status()}`
);

// ------------------------------------------------------------------------ the views
//
// Two panes for one session: the console as this page renders it, and the guest's own display.
// One at a time, and both stay live while hidden — the guest draws whether or not anyone is
// looking, and the terminal keeps its line — which is what lets the toggle be checked without
// disturbing anything.

const paneOf = (id) => document.getElementById(id);
const viewOf = (which) => document.getElementById(`view-${which}`);

check(
  'the page opens on the console, with the display hidden',
  paneOf('screen').hidden === false &&
    paneOf('display').hidden === true &&
    viewOf('terminal').dataset.active === 'true',
  `screen hidden=${paneOf('screen').hidden} display hidden=${paneOf('display').hidden}`
);

viewOf('display').click();
check(
  'the toggle switches to the display and says which pane is showing',
  paneOf('display').hidden === false &&
    paneOf('screen').hidden === true &&
    viewOf('display').dataset.active === 'true' &&
    viewOf('terminal').dataset.active === 'false' &&
    viewOf('display').getAttribute('aria-pressed') === 'true',
  `display hidden=${paneOf('display').hidden} active=${viewOf('display').dataset.active} ` +
    `aria-pressed=${viewOf('display').getAttribute('aria-pressed')}`
);

viewOf('terminal').click();
check(
  'and back: the console was live the whole time, so its prompt is still there',
  paneOf('screen').hidden === false && paneOf('display').hidden === true && screen().includes('# '),
  `screen hidden=${paneOf('screen').hidden} last=${JSON.stringify(screen().slice(-20))}`
);

// The symptom this page must not have: a row rendered twice. The composition is two disjoint
// slices of one line and the line above it, so a duplicate can only mean the model was read
// twice — which is what a reader reported once, and what this pins down.
const occurrences = (haystack, needle) => haystack.split(needle).length - 1;

for (const ch of 'abc') press(ch);
press('Enter');
await untilStable(50, 5000);
check(
  'the line the reader typed is on the screen once, not twice',
  occurrences(screen(), '# abc') === 1,
  `occurrences=${occurrences(screen(), '# abc')}\nscreen:\n${screen()}`
);

// ----------------------------------------------------------------------- the cursor
//
// The renderer has always tracked the cursor's column — the shell's editor moves it with `\b` and
// `\r` — and nothing drew it. What the page draws is the CSS block between the text before the
// column and the text after it, so the cursor element's *own* text is what the shell has ahead of
// its cursor: that is what these check.

const cursorPart = () => document.getElementById('cursor');
const cursorAfter = () => cursorPart().textContent;
const cursorIsLast = () => {
  const children = paneOf('screen').children;
  return children[children.length - 1] === cursorPart();
};

for (const ch of 'echo cursortest') press(ch);
check(
  'the terminal draws a cursor at the end of the line being typed',
  // What is ahead of the cursor is whitespace, and that is not a bug: the shell's editor blanks the
  // rest of the row with spaces and then steps back over them, so the model has written a run of
  // blanks that the cursor sits in *front* of. What the line says is on the other side of it.
  (await until(() => screen().includes('echo cursortest'), 'the typed line')) &&
    cursorIsLast() &&
    /^\s*$/.test(cursorAfter()),
  `after-cursor=${JSON.stringify(cursorAfter().slice(0, 20))}… last=${JSON.stringify(screen().slice(-20))}`
);

press('ArrowLeft');
check(
  'the cursor is the shell\'s, not the end of the text: arrow-left leaves a character ahead of it',
  await until(() => /^t\s*$/.test(cursorAfter()), 'the cursor to move back'),
  `after-cursor=${JSON.stringify(cursorAfter().slice(0, 20))}…`
);

press('u', { ctrlKey: true });
check(
  'killing the line takes the text out from before the cursor, not the cursor',
  await until(
    () => !screen().includes('cursortest') && /^\s*$/.test(cursorAfter()),
    'the killed line'
  ),
  `after-cursor=${JSON.stringify(cursorAfter().slice(0, 20))}… last=${JSON.stringify(screen().slice(-20))}`
);

// ------------------------------------------------------------------------ the disk
//
// The page's block device is IndexedDB, and this is where that is checked — through the
// store's own interface, so what is asserted is what a reload would see: `indexedDbStore`
// over the database the page is writing to, answered with the image plus the records the
// guest put there. Nothing here reads the fake's internals; the fake is the browser's
// IndexedDB replaced, not the page's code.

const IMAGE = ARTIFACT_BYTES['build/minixfs.img'];
/// `s_flags` in the MinixFS superblock, and the bit that says the last writer finished.
/// A freshly built image has it set; the first read-write mount clears it.
const SUPERBLOCK_FLAGS = 1024 + 18;
const flagsOf = (bytes) => bytes[SUPERBLOCK_FLAGS] | (bytes[SUPERBLOCK_FLAGS + 1] << 8);
const contains = (bytes, text) =>
  Buffer.from(bytes.buffer, bytes.byteOffset, bytes.length).includes(Buffer.from(text, 'utf8'));

const reopened = async (name) => indexedDbStore({ name, imageBytes: IMAGE });

const booted = await reopened('minixrs-disk');
// At a parked prompt the store holds the image id and no pages: the mount cleared the clean
// bit in MFS's *cache*, and nothing has flushed it yet because nothing has synced — which is
// finding 49, and the reason a session's writes are the shutdown's business.
check(
  'the store is seeded for this image, and has no bytes of its own yet',
  booted.imageId !== null && (indexedDB.databases.get('minixrs-disk')?.tables.get('pages').size ?? 0) === 0,
  `imageId=${booted.imageId}`
);
check(
  'the page says where its disk is, so a reader knows a reload will find their files',
  diskLabel().includes('IndexedDB'),
  `disk: ${JSON.stringify(diskLabel())}`
);
check(
  'the page took the lock on the disk, so this tab is the one writing it',
  locks.held.has('minixrs-disk-owner'),
  `held: ${[...locks.held].join(' ') || '(nothing)'}`
);

// What the terminal cannot show is whether any of it reached the disk: a ramdisk boot
// looks exactly the same from here, and that is the whole difference the page's disk makes.
const PAGE_FILE = '/page-disk.txt';
const PAGE_TEXT = 'written by the page, through the filesystem, to IndexedDB';
for (const ch of `echo ${PAGE_TEXT} > ${PAGE_FILE}`) press(ch);
press('Enter');
for (const ch of `cat ${PAGE_FILE}`) press(ch);
press('Enter');
check(
  'a file the page writes comes back through the page, so the redirect reached the filesystem',
  await until(() => screen().split('\n').includes(PAGE_TEXT), 'the file contents'),
  `screen:\n${screen()}`
);

// The session ends the way a tab ends it: the control, not the keyboard. It is the same bytes
// the shell would get (`^U` and `exit`, which is INIT's exit — the shutdown M4 exists to run),
// and the reason the control exists is on the other side of it: a session that ends this way
// leaves a disk the next boot can write to.
//
// A half-typed line is left there on purpose before the control is pressed: without the `^U`
// `endSession` sends first, that line and `exit` become one command and nothing ends.
for (const ch of 'echo not this') press(ch);
const asked = document.getElementById('shutdown').click();
check(
  'the shutdown control ends the session: the guest runs out of processes, and the disk is clean',
  asked && (await until(() => status().includes('the session ended'), 'the end of the session')),
  `asked=${asked}\nstatus: ${status()}`
);
check(
  'the shutdown control discarded the half-typed line rather than running it',
  !screen().includes('not this'),
  `screen:\n${screen()}`
);
check(
  'the page says so on its disk line, so a reader knows this tab can be closed',
  diskLabel().includes('clean'),
  `disk: ${JSON.stringify(diskLabel())}`
);

const after = await reopened('minixrs-disk');
const disk = after.read(0, IMAGE.length);
check(
  'a store reopened over the same IndexedDB answers with the file the page wrote, so the writes went to the disk and not to a ramdisk',
  contains(disk, PAGE_TEXT),
  `the disk does not hold ${JSON.stringify(PAGE_TEXT)}`
);
check(
  'the page left the filesystem marked clean, so the next boot mounts it read-write',
  flagsOf(disk) === 1,
  `disk s_flags=${flagsOf(disk)}`
);
check(
  'the store remembers which image its contents were made from',
  after.imageId !== null && after.imageId === booted.imageId,
  `before=${booted.imageId} after=${after.imageId}`
);

// The one failure mode a store adds over a real disk: it outlives the image it was seeded
// from, so a rebuilt image over an old disk would be two filesystems mixed. The device
// refuses to attach rather than mixing them, which is the loud half of that choice.
const stale = await indexedDbStore({ name: 'a-stale-disk', imageBytes: IMAGE });
stale.setImageId('made from some other image');
const refused = [];
const guardHost = createHost({
  kernel: ARTIFACT_BYTES['build/kernel.wasm'],
  servers: ARTIFACT_BYTES['build/servers.async.wasm'],
  image: IMAGE,
  store: stale,
  sink: { write() {}, note: (name, detail) => refused.push(`${name}: ${detail}`) },
});
check(
  'a disk whose contents came from another image is refused, not mixed',
  guardHost.device.attached === false && refused.some((n) => n.includes('different image')),
  `attached=${guardHost.device.attached} reports=${refused.join(' | ')}`
);

// A bring-up that cannot work for the other reason: a spec the module does not export. That is
// what a *stale* set of artifacts looks like from the host's side — the page fetches its own staged
// copies, so a spec list from after a build and modules from before it is a real state to be in, and
// it has to fail as a sentence naming the entry rather than a TypeError trapped inside a slot.
let missingExport = null;
try {
  createHost({
    kernel: ARTIFACT_BYTES['build/kernel.wasm'],
    servers: ARTIFACT_BYTES['build/servers.async.wasm'],
    image: IMAGE,
    specs: [...SYSTEM_SPECS, { slot: 24, entry: 'minix_server_absent', label: 'absent' }],
  });
} catch (error) {
  missingExport = error.message;
}
check(
  'a spec the module does not export fails the boot, naming the entry and the fix',
  // The loaded sizes are in the message because that is the question this failure always raises
  // (which build is the page running?), and they are the one thing the page can answer itself.
  missingExport !== null &&
    missingExport.includes('minix_server_absent') &&
    missingExport.includes('tools/wasm-browser/build.sh') &&
    missingExport.includes(String(ARTIFACT_BYTES['build/servers.async.wasm'].length)),
  `error=${missingExport}`
);

// A browser with no IndexedDB at all (a private window, typically) cannot be simulated in
// this process — the page is imported once and is already running — so what is checked is
// that the store says so rather than failing later: `page.js` turns this into a diskless
// boot and a note, which is the configuration the harness runs.
check(
  'a browser with no IndexedDB is refused by the store rather than half-working',
  await indexedDbStore({ imageBytes: IMAGE, indexedDB: null }).then(
    () => false,
    (error) => error.message.includes('IndexedDB')
  ),
  'indexedDbStore did not refuse a missing indexedDB'
);

// ------------------------------ a disk the page will not keep, and the way out of it
//
// The other state with no way out from the prompt: a store whose contents came from a different
// image (the device is never attached) and a filesystem a tab left unclean (read-only until it
// is cleared). Starting over is the page's answer to both, and what it has to do is throw the
// database away — closing its own connection first, or the delete blocks on itself — and boot
// from the image again.
const clear = document.getElementById('clear');
const cleared = clear.click();
check(
  'starting over clears the disk and reloads, which is the way out of a disk the store refuses',
  cleared && (await until(() => reloads === 1, 'the reload')),
  `clicked=${cleared} reloads=${reloads}`
);
const fresh = await indexedDbStore({ name: 'minixrs-disk', imageBytes: IMAGE });
check(
  'a store over the cleared database is unseeded, so the next boot starts from the image again',
  fresh.imageId === null &&
    (indexedDB.databases.get('minixrs-disk')?.tables.get('pages').size ?? 0) === 0,
  `imageId=${fresh.imageId} pages=${indexedDB.databases.get('minixrs-disk')?.tables.get('pages').size}`
);

// --------------------------------------------------------------------- the second tab

// What a tab that cannot have the disk sees. `page.js` is imported a second time, which is what a
// second tab is: the same process-level fakes, the same lock (taken by the first import, so this
// one is answered with `null`), and the page's own bring-up deciding what to do about it. That
// makes this the check for the page's *no disk* path as well — the one a reader reaches without
// doing anything unusual, where the boot has to continue rather than fail.
await import('./page.js?second-tab');
check(
  'a second tab is refused the disk and boots from the ramdisk instead',
  await until(() => diskLabel().includes('none'), 'the diskless label'),
  `disk: ${JSON.stringify(diskLabel())}`
);
check(
  'the second tab says why on the page, rather than only in a report it has to be asked for',
  document.getElementById('panel').textContent.includes('another tab has the disk open'),
  `panel: ${JSON.stringify(document.getElementById('panel').textContent)}`
);
// What is left is whether this boot *works*: the import sets the status to 'loading the kernel…'
// itself, so a page that never got past bring-up would sit there — a denied lock that the page
// waited on would hang rather than fail, and this is the check that notices.
check(
  'the second tab reaches a prompt, so a disk it cannot have is not a page that fails',
  await until(() => status().includes('waiting for input'), 'the parked status'),
  `status: ${status()}\ndisk: ${JSON.stringify(diskLabel())}`
);

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
