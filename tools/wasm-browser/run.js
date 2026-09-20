// Drive the page's host engine from Node.
//
//   node tools/wasm-browser/run.mjs
//
// This exists because a page cannot be tested by a script — it needs a human, a keystroke and a
// canvas — so the parts worth testing are factored out of it. `host.mjs` is the engine,
// `terminal.mjs` is the renderer, and `page.mjs` is a DOM adapter over the two; this drives the
// same modules with a scripted keystroke sequence instead of a keyboard and asserts what reached
// the console.
//
// What it therefore does not cover, and the page does: the DOM rendering, the keyboard mapping,
// and the scheduling of slices against the browser's event loop. Those are the parts to read in
// `page.mjs` rather than to trust from here.
//
// The policy below is the page's policy, not a test-only one: pump a slice, and if the slice did
// nothing but retry the console read with an empty input queue, park until there is something to
// read. That park is what keeps an idle page off the CPU, so the fact that this harness parks at
// both prompts is the fact that matters for the page.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { createHost, SYSTEM_SPECS } from './host.js';
import { createTerminal } from './terminal.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const buildDir = path.resolve(here, '../wasm-servers/build');
const imagePath = path.join(root, 'target/images/wasm32-minix/minixfs.img');

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail !== undefined && (!ok || process.env.WASM_VERBOSE === '1')) {
    console.log(`        ${detail}`);
  }
}
function note(name, detail) {
  console.log(`NOTE  ${name}`);
  if (detail !== undefined) console.log(`        ${detail}`);
}

const INIT_SLOT = SYSTEM_SPECS.find((s) => s.label === 'init').slot;

const terminal = createTerminal();
const diagnostics = [];
const host = createHost({
  kernel: fs.readFileSync(path.join(buildDir, 'kernel.wasm')),
  servers: fs.readFileSync(path.join(buildDir, 'servers.async.wasm')),
  image: fs.readFileSync(imagePath),
  sink: {
    write: (byte) => terminal.write(byte),
    note: (name, detail) =>
      diagnostics.push(`${name}${detail === undefined ? '' : `: ${detail}`}`),
  },
});

// ------------------------------------------------------------------------ the pump

let slices = 0;

function settle({ maxSlices = 20000 } = {}) {
  for (let i = 0; i < maxSlices; i += 1) {
    slices += 1;
    const reason = host.pump({ maxSyscalls: 400 });
    if (reason !== 'slice') return reason;
    if (host.sliceWasSpinOnly() && host.console.pending === 0) return 'awaiting-input';
  }
  return 'slices-exhausted';
}

const type = (text) => host.console.push(text);

/// Where every process is, for a check that fails: which slot, its label, whether the kernel
/// thinks it is blocked, and the last few syscalls it made.
function describe() {
  return host.procs
    .map((p) => {
      const blocked = host.kernel.exports.minix_proc_blocked(p.spec.slot);
      const tail = p.tail.map((t) => `nr=${t.nr}`).join(',');
      return (
        `          | ${p.spec.label} (slot ${p.spec.slot}): blocked=${blocked} ` +
        `syscalls=${p.syscalls} exited=${p.exited} tail=[${tail}]`
      );
    })
    .join('\n');
}

// ------------------------------------------------------------------------- the run

const booted = settle();
check(
  'the system boots to a prompt on its own',
  booted === 'awaiting-input' && terminal.text().includes('# '),
  `settle ended on ${booted}; console:\n${terminal.text()}`
);

// The park is this engine's reason for existing: a shell waiting for input must not be a spin the
// host cannot interrupt. If the engine could not stop the guest here, `settle` would have run its
// slices out instead, so this pins the mechanism rather than the outcome.
check(
  'the guest parked at the prompt instead of spinning inside one dispatch',
  booted === 'awaiting-input' && host.slice.used > 0 && host.slice.spin === host.slice.used,
  `last slice: used=${host.slice.used}, of which spin=${host.slice.spin}`
);

type('/bin/echo hi\n');
const afterEcho = settle();
const forked = host.forks.find((f) => f.parent === INIT_SLOT);
check(
  "a typed command the shell cannot answer itself forked, and the child exec'd the image's module",
  forked !== undefined &&
    forked.st.forkOf === INIT_SLOT &&
    forked.st.exec !== undefined &&
    forked.st.exec.path === '/bin/echo' &&
    forked.st.exec.argv[0] === '/bin/echo',
  forked === undefined
    ? `no fork by slot ${INIT_SLOT}; console:\n${terminal.text()}\n${describe()}`
    : `child slot=${forked.child} path=${forked.st.exec && forked.st.exec.path} ` +
      `argv=${JSON.stringify(forked.st.exec && forked.st.exec.argv)}`
);
check(
  "the child's output reached the console and the shell came back for more",
  afterEcho === 'awaiting-input' && terminal.lines.includes('hi'),
  `settle ended on ${afterEcho}; console:\n${terminal.text()}`
);

type('exit\n');
const afterExit = settle();
check(
  'the shell exits and the system reaches quiescence, with nothing left spinning',
  afterExit === 'quiescent',
  `settle ended on ${afterExit}`
);

// ---------------------------------------------------------------------- the report

note(
  'what this run cost',
  `${slices} slices, ${host.steps} dispatch steps, ` +
    `${host.budget.left} syscalls of budget left, ${host.forks.length} fork(s)`
);
if (diagnostics.length > 0) note('host reports', diagnostics.join('\n        '));

console.log('\n--- console ---');
for (const l of terminal.lines.filter((l) => l.length > 0)) console.log(`  kernel: ${l}`);
if (terminal.partial !== '') console.log(`  kernel: ${terminal.partial} (partial)`);

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
