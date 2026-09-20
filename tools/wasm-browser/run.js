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

// ----------------------------------------------- a loop of commands, and its cost
//
// The page can drive a loop now, and the cost question is the one the M3b note leaves open: a fork
// clones the whole instance rather than just a stack. That is a *number per command* — how much
// moves across the seam, how many slices and dispatch steps it takes — and the thing worth
// pinning is that it does not grow. A loop whose per-command cost climbs with the iteration count
// is a loop that stops working; a loop that is uniformly expensive is just slow, which is a
// property of the design and not of the implementation.
//
// The command is the same external `/bin/echo` the check above uses, so every iteration is a
// fork + exec + reap through the same chain, and the output is what says it ran.
const LOOP_COMMANDS = 5;
const loop = [];
for (let i = 0; i < LOOP_COMMANDS; i += 1) {
  const before = {
    slices,
    steps: host.steps,
    copies: host.copies.count,
    bytes: host.copies.bytes,
    forks: host.forks.length,
  };
  type(`/bin/echo loop-${i}\n`);
  const settled = settle();
  const fork = host.forks[host.forks.length - 1];
  loop.push({
    settled,
    slices: slices - before.slices,
    steps: host.steps - before.steps,
    copies: host.copies.count - before.copies,
    bytes: host.copies.bytes - before.bytes,
    forks: host.forks.length - before.forks,
    child: fork === undefined ? null : fork.child,
    parentBytes: fork === undefined ? null : fork.parentBytes,
    cloneMs: fork === undefined ? null : Math.round(fork.cloneMs * 100) / 100,
  });
}

check(
  'every command in the loop ran, and the shell came back for the next one',
  loop.every((it) => it.settled === 'awaiting-input') &&
    Array.from({ length: LOOP_COMMANDS }, (_, i) => `loop-${i}`).every((line) =>
      terminal.lines.includes(line)
    ),
  loop
    .map((it, i) => `${i}: settled=${it.settled} forks=${it.forks}`)
    .join('; ') + `\n${terminal.text()}`
);

// One fork per command, and every one of them into the *same* slot: a child that is reaped frees
// its slot, and PM hands the freed one out again. A loop that took a new slot each time would run
// out of the port's 256 of them, and would show up here as a distinct child slot per iteration.
const childSlots = new Set(loop.map((it) => it.child));
check(
  'each command forked once, into the one slot the previous child freed by being reaped',
  loop.every((it) => it.forks === 1) && childSlots.size === 1 && !childSlots.has(null),
  `forks per command=[${loop.map((it) => it.forks).join(', ')}], child slots=[${loop
    .map((it) => it.child)
    .join(', ')}]`
);

// The cost, as a bound rather than as a benchmark: the point is that it is flat, so a command in the
// middle of the loop may not cost an order of magnitude more than the first one. The numbers
// themselves are in the note below, where a reader can see what "flat" was measured to mean
// instead of having to trust this factor.
const firstCost = loop[0];
const worstCost = loop.reduce((a, b) => (b.bytes > a.bytes ? b : a), loop[0]);
check(
  'the cost per command does not grow with the iteration count',
  loop.every((it) => it.bytes > 0 && it.bytes <= firstCost.bytes * 3),
  `bytes per command=[${loop.map((it) => it.bytes).join(', ')}], ` +
    `worst=${worstCost.bytes} first=${firstCost.bytes}`
);

note(
  `what ${LOOP_COMMANDS} commands in a row cost`,
  'iteration: slices steps seam-copies seam-bytes clone clone-MiB child-slot\n' +
    loop
      .map(
        (it, i) =>
          `          ${i}: ${it.slices} ${it.steps} ${it.copies} ${it.bytes} ` +
          `${it.cloneMs} ms ${(it.parentBytes / (1024 * 1024)).toFixed(0)} slot ${it.child}`
      )
      .join('\n') +
    '\n        The seam totals are the messages; the clone is the fork, and it is the whole cost of a\n' +
    '        command — the same instance, byte for byte, whether or not the command needs it.'
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
