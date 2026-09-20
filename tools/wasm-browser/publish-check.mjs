// Say whether the published copy of the demo actually works.
//
//   node tools/wasm-browser/publish-check.mjs        (run by `publish.sh`)
//
// `publish.sh` stages the page into `docs/`, which is the directory GitHub Pages serves. A
// staged site can fail in ways nothing else in this repository would notice, so this checks the
// three that matter:
//
//   * **the copies are the tested files, byte for byte.** This is the same argument
//     `tools/wasm-servers/build.sh` makes about the artifacts ("a page built by a different
//     pipeline from the ones the checks ran would be a page nobody had tested"), applied to the
//     source files — which is what makes `page.test.js`'s 42 checks a statement about the demo.
//   * **the site is portable.** Pages serves it under `/<repo>/`, so a root-absolute URL is a
//     404 for everyone but the author, who is probably serving `docs/` at `/`.
//   * **it boots from the published tree** — `docs/host.js` driving the guest out of
//     `docs/build/`. That is the only check that proves the copy is complete, and it is the one
//     that would have caught a missing module or a stale artifact before a reader did.
//
// It runs headless: the display is a recorder, the way `boot.cjs` does it, so what is asserted
// about the guest's pixels is what its own `fb` driver presented.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const site = path.join(root, 'docs');

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail !== undefined && !ok) console.log(`        ${detail}`);
}

/// What the page is, and what it fetches. Kept here rather than discovered, so a file that
/// stops being copied is a failing check rather than a page nobody notices is broken.
const SITE_FILES = ['index.html', 'page.js', 'host.js', 'display.js', 'store.js', 'terminal.js'];
const ARTIFACTS = ['kernel.wasm', 'servers.async.wasm', 'minixfs.img'];

const staged = (relative) => path.join(site, relative);
const read = (file) => (fs.existsSync(file) ? fs.readFileSync(file) : null);

// ------------------------------------------------------------------ staged at all

if (!fs.existsSync(site)) {
  console.log(`no docs/ at ${site} — stage it first:\n\n  sh tools/wasm-browser/publish.sh\n`);
  process.exit(1);
}

// --------------------------------------------------- the copies are the tested files

for (const file of SITE_FILES) {
  const source = read(path.join(here, file));
  const copy = read(staged(file));
  check(
    `${file} is staged as the file the checks run`,
    copy !== null && source !== null && source.length > 0 && source.equals(copy),
    copy === null ? 'not staged' : `source ${source.length} B, staged ${copy.length} B`
  );
}

// ---------------------------------------------------------------- the artifacts

for (const file of ARTIFACTS) {
  const copy = read(staged(`build/${file}`));
  check(
    `build/${file} is staged and not empty`,
    copy !== null && copy.length > 0,
    copy === null ? 'not staged' : `${copy.length} B`
  );
}

// ---------------------------------------------------------------------- portability

const html = read(staged('index.html'))?.toString('utf8') ?? '';
const absolute = [...html.matchAll(/(?:src|href)="(\/[^/"][^"]*)"/g)].map((m) => m[1]);
check(
  'index.html names no root-absolute URLs, so it works under /<repo>/ on Pages',
  absolute.length === 0,
  `found ${absolute.join(', ')}`
);

const modules = SITE_FILES.filter((f) => f.endsWith('.js'));
const relativeImports = [];
for (const file of modules) {
  const text = read(staged(file))?.toString('utf8') ?? '';
  for (const m of text.matchAll(/from\s+'([^']+)'/g)) {
    if (m[1].startsWith('.')) relativeImports.push([file, m[1]]);
  }
}
const missing = relativeImports.filter(([, spec]) => !fs.existsSync(staged(spec.slice(2))));
check(
  'every module the site imports is staged next to it',
  missing.length === 0,
  missing.map(([file, spec]) => `${file} → ${spec}`).join('; ')
);

// --------------------------------------------------------- it boots from the staged tree

const { createHost } = await import(pathToFileURL(staged('host.js')).href);
const { createTerminal } = await import(pathToFileURL(staged('terminal.js')).href);

const display = {
  width: 1024,
  height: 768,
  frames: 0,
  first: null,
  last: null,
  present(bytes) {
    this.frames += 1;
    const frame = Uint8Array.from(bytes);
    if (this.first === null) this.first = frame;
    this.last = frame;
  },
};

const terminal = createTerminal();
const host = createHost({
  kernel: read(staged('build/kernel.wasm')),
  servers: read(staged('build/servers.async.wasm')),
  image: read(staged('build/minixfs.img')),
  display,
  sink: { write: (byte) => terminal.write(byte), note: () => {} },
});

/// `run.js`'s settle: pump until the guest has nothing to do but retry the console read, or a
/// prompt would burn the whole syscall budget spinning.
function settle(limit = 4000) {
  for (let i = 0; i < limit; i += 1) {
    const reason = host.pump({ maxSyscalls: 400 });
    if (reason !== 'slice') return reason;
    if (host.sliceWasSpinOnly() && host.console.pending === 0) return 'awaiting-input';
  }
  return 'steps';
}

const booted = settle();
check(
  'the staged system boots to a prompt',
  booted === 'awaiting-input' && terminal.text().includes('# '),
  `stopped with ${booted}\n${terminal.text()}`
);

host.console.push('echo published\n');
settle();
check(
  'a command typed at the staged prompt runs and answers',
  terminal.lines.includes('published'),
  `console:\n${terminal.text()}`
);

/// The pixel at `(x, y)` of a frame the guest presented, in the guest's own channel order.
const pixelOf = (frame, x, y) => {
  const at = (y * display.width + x) * 4;
  return [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]];
};

check(
  "the staged guest's display reached the host: the driver's pattern, XRGB8888",
  // The driver's own three bands, which is the *first* frame `fb` presents at boot. The last frame
  // is the compositor's desktop — checked next — because the console's window replaces it (M5b).
  display.frames > 0 &&
    pixelOf(display.first, 100, 400).join() === '0,0,255,0' &&
    pixelOf(display.first, 500, 400).join() === '0,255,0,0' &&
    pixelOf(display.first, 900, 400).join() === '255,0,0,0',
  display.frames === 0
    ? 'no frame was presented'
    : `left=${pixelOf(display.first, 100, 400)} ` +
      `middle=${pixelOf(display.first, 500, 400)} right=${pixelOf(display.first, 900, 400)}`
);
check(
  "the staged guest's compositor put its desktop on the display",
  // The console's window, composed by `wserver` and copied into `/dev/fb` as one datagram write
  // (M5b): the desktop background outside it, the focused title bar's colour in its title, and the
  // body's colour in its body.
  display.last !== null &&
    pixelOf(display.last, 20, 20).join() === '40,40,40,0' &&
    pixelOf(display.last, 400, 190).join() === '192,128,64,0' &&
    pixelOf(display.last, 300, 300).join() === '32,24,24,0',
  display.last === null
    ? 'no frame was presented'
    : `desktop=${pixelOf(display.last, 20, 20)} title=${pixelOf(display.last, 400, 190)} ` +
      `body=${pixelOf(display.last, 300, 300)}`
);

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
