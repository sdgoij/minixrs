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
  for (const file of ['page.js', 'host.js', 'terminal.js']) {
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
    this.textContent = '';
    this.dataset = {};
    this.hidden = false;
    this.scrollTop = 0;
    this.scrollHeight = 0;
  }
}
globalThis.document = {
  getElementById(id) {
    if (!elements.has(id)) elements.set(id, new StubElement(id));
    return elements.get(id);
  },
};

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

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
