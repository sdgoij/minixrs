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
import { fileStore } from './file-store.js';
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

// Read once and shared: the M4 section below boots the system a second time from the same three
// artifacts, and an engine that read them again could be reading a different build.
const kernelWasm = fs.readFileSync(path.join(buildDir, 'kernel.wasm'));
const serversWasm = fs.readFileSync(path.join(buildDir, 'servers.async.wasm'));
const imageBytes = fs.readFileSync(imagePath);

const terminal = createTerminal();
const diagnostics = [];
const host = createHost({
  kernel: kernelWasm,
  servers: serversWasm,
  image: imageBytes,
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

// --------------------------------------------- M4: a disk, and what survives a boot
//
// Everything above this line runs diskless: the host hands the RAM disk the boot filesystem image
// at every boot, so the system mounts an immutable copy and nothing a run writes can outlive it.
// M4 attaches a *device* instead — the host owns the bytes of a real block device, `virtio_blk`
// finds it instead of finding nothing, and MFS mounts the root from it.
//
// The claim to check is the one a filesystem makes and a RAM disk cannot: **a file written in one
// boot is there in the next.** Two engines, one store, and the second one asks the shell to `cat`
// what the first one wrote. A store that was silently reseeded, or an image mounted instead of the
// disk, fails this — and so does a disk whose superblock, inodes and data blocks do not all agree
// about where the file is, which is what makes this a test of the whole path rather than of the
// write alone.
//
// The second claim here is the shutdown's, and it is the one that made finding 49: **the disk is
// left clean when the system stops.** `MFSFLAG_CLEAN` on the superblock is what lets the *next*
// mount be read-write — MFS refuses to write on a filesystem whose last user did not end cleanly,
// which is its protection against writing over a half-written one — so a shutdown that does not
// flush and unmount leaves a disk that reads fine and refuses to be written, and nothing says why.
// The third and fourth boots below write files and never sync them: what makes those writes
// durable is the shutdown, and nothing else.

/// Boot the system over `store`, type `lines`, and hand back what reached the console.
///
/// The pump policy is the one above, restated because this drives a second engine: a slice, and a
/// park when the slice did nothing but retry the console read. Nothing here is a check — the two
/// calls that follow read the result.
function bootOver(store, lines) {
  const term = createTerminal();
  const engine = createHost({
    kernel: kernelWasm,
    servers: serversWasm,
    image: imageBytes,
    store,
    sink: {
      write: (byte) => term.write(byte),
      note: (name) => notes.push(name),
    },
  });
  const notes = [];
  let steps = 0;
  const run = () => {
    for (let i = 0; i < 20000; i += 1) {
      steps += 1;
      const why = engine.pump({ maxSyscalls: 400 });
      if (why !== 'slice') return why;
      if (engine.sliceWasSpinOnly() && engine.console.pending === 0) return 'awaiting-input';
    }
    return 'slices-exhausted';
  };
  const booted = run();
  for (const line of lines) {
    engine.console.push(`${line}\n`);
    run();
  }
  engine.console.push('exit\n');
  const ended = run();
  return { terminal: term, engine, booted, ended, steps, notes };
}

const diskPath = process.env.M4_STORE ?? path.join(root, 'target/wasm-disk-test.img');
for (const stale of [diskPath, `${diskPath}.json`]) {
  if (fs.existsSync(stale)) fs.unlinkSync(stale);
}
const store = fileStore(diskPath, imageBytes);

const PERSIST_FILE = 'persisted.txt';
const PERSIST_TEXT = 'written by the first boot';
const UNSYNCED_FILE = 'unsynced.txt';
const UNSYNCED_TEXT = 'written without syncing';
const AGAIN_FILE = 'again.txt';
const AGAIN_TEXT = 'written by a boot after a shutdown';

// `sync` before `exit` here on purpose: this boot is the M4 claim on its own — a write the *guest*
// flushed and the shutdown had no part in — and it keeps the shell's `sync` builtin exercised.
// `fs_sync` writes the dirty inodes and flushes the block cache, so the file this boot makes
// durable is the one the next boot reads.
const firstBoot = bootOver(store, [`echo ${PERSIST_TEXT} > ${PERSIST_FILE}`, 'sync']);
check(
  'the host attached a block device, and the first boot mounted its root from it',
  firstBoot.engine.device.attached &&
    firstBoot.engine.device.bytesRead > 0 &&
    firstBoot.ended === 'quiescent',
  `attached=${firstBoot.engine.device.attached} reads=${firstBoot.engine.device.reads} ` +
    `bytesRead=${firstBoot.engine.device.bytesRead} ended=${firstBoot.ended}`
);
// Boot 1's own evidence is that bytes reached the device and that the shell did not complain: a
// redirect that failed prints `cannot create`, and a write that failed inside the filesystem would
// leave the device's write count at zero. What the file *is* takes the second boot to check.
check(
  "the shell's redirect wrote through the filesystem to the device",
  firstBoot.engine.device.bytesWritten > 0 &&
    !firstBoot.terminal.lines.some((l) => l.includes('cannot create')) &&
    firstBoot.ended === 'quiescent',
  `writes=${firstBoot.engine.device.writes} bytesWritten=${firstBoot.engine.device.bytesWritten} ` +
    `console=[${firstBoot.terminal.lines.join(' | ')}]`
);
store.close();

// The second boot opens the same store. Nothing about the image changed between the two calls, so
// anything it can see that the first run wrote came off the disk.
const secondBoot = bootOver(fileStore(diskPath, imageBytes), [`cat ${PERSIST_FILE}`]);
const seen = secondBoot.terminal.lines.find((l) => l.trim() === PERSIST_TEXT);
check(
  'a second boot sees the file the first one wrote, so the disk is the one that persisted',
  secondBoot.booted === 'awaiting-input' && seen !== undefined,
  `booted=${secondBoot.booted} lines=[${secondBoot.terminal.lines.join(' | ')}]`
);
store.close();

// The third boot writes and does *not* sync: at the moment it exits the inode, the directory block
// and the data block are still in MFS's cache, so whether a later boot can read the file is a
// question about the shutdown alone. It is also the boot that catches finding 49 in the other
// direction: a disk left unclean by the second boot's shutdown mounts read-only, and a read-only
// mount writes nothing at all — so a device that saw no writes *is* the shell's `cannot create`.
const thirdBoot = bootOver(fileStore(diskPath, imageBytes), [
  `echo ${UNSYNCED_TEXT} > ${UNSYNCED_FILE}`,
]);
check(
  'a boot after a shutdown mounted the disk read-write, and its write reached the device',
  thirdBoot.engine.device.bytesWritten > 0 &&
    !thirdBoot.terminal.lines.some((l) => l.includes('cannot create')) &&
    thirdBoot.ended === 'quiescent',
  `writes=${thirdBoot.engine.device.writes} bytesWritten=${thirdBoot.engine.device.bytesWritten} ` +
    `console=[${thirdBoot.terminal.lines.join(' | ')}]`
);
store.close();

// The fourth boot reads back what the third one wrote without syncing — the shutdown flushed it, or
// the file would still have been in MFS's cache when the host stopped — and writes a file of its
// own, which is the same question about the disk's writability asked of the third boot's shutdown.
const fourthBoot = bootOver(fileStore(diskPath, imageBytes), [
  `cat ${UNSYNCED_FILE}`,
  `echo ${AGAIN_TEXT} > ${AGAIN_FILE}`,
]);
const flushed = fourthBoot.terminal.lines.find((l) => l.trim() === UNSYNCED_TEXT);
check(
  'a boot that exited without syncing still left its file on the disk',
  fourthBoot.booted === 'awaiting-input' && flushed !== undefined,
  `booted=${fourthBoot.booted} lines=[${fourthBoot.terminal.lines.join(' | ')}]`
);
store.close();

note(
  'what the M4 device cost and saw',
  [firstBoot, secondBoot, thirdBoot, fourthBoot]
    .map(
      (b, i) =>
        `${['first', 'second', 'third', 'fourth'][i]} boot: ${b.engine.device.reads} reads / ` +
        `${b.engine.device.bytesRead} bytes, ${b.engine.device.writes} writes / ` +
        `${b.engine.device.bytesWritten} bytes`
    )
    .join('\n        ') +
    `\n        (${diskPath})`
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
