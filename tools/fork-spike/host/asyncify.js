'use strict';
//
// Fork spike, layer 2: the real thing, with Binaryen's Asyncify.
//
// Layer 1 tested the host-side clone mechanics with a hand-written continuation.
// This layer tests the actual mechanism the port would use: a process suspended
// inside a blocking syscall, with Asyncify having serialised its call stack into
// linear memory, forked by cloning that memory into a second instance.
//
// The protocol below was read out of the instrumented module rather than
// assumed. `wasm-opt --asyncify` adds exactly two mutable i32 globals
// (state and a pointer to an asyncify_data struct) and exports four hooks. The
// only writes to the state global are inside those hooks, so the whole state
// machine is host-drivable and the guest needs no cooperation at all:
//
//   asyncify_start_unwind(ptr)   state = 1 (UNWINDING)
//   asyncify_start_rewind(ptr)   state = 2 (REWINDING)
//   asyncify_stop_rewind()       state = 0 (NORMAL)
//
// That matters for the port: the kernel is the entity that knows a syscall must
// block, and it is already the entity implementing the syscall gate. The guest
// stays ordinary straight-line code.

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const PAGE = 65536;
const MIN_PAGES = 64; // must be >= the guest's --initial-memory (4 MiB)
const BUF_SIZE = 65536; // asyncify stack buffer, placed at __heap_base
const STRUCT_SIZE = 16; // {stack_ptr, stack_end, stack_start} + slack
const PARENT_REPLY = 4242; // stands in for the child's pid
const STATE_NORMAL = 0;
const STATE_UNWINDING = 1;
const STATE_REWINDING = 2;

const buildDir = path.join(__dirname, '..', 'build');
const baseWasm = path.join(buildDir, 'guest.wasm');
const asyncWasm = path.join(buildDir, 'guest.async.wasm');

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail) console.log(`        ${detail}`);
}
// Observations that are not pass/fail: the spike's job is to discover how the
// mechanism behaves at its edges, not only to confirm the happy path.
function note(name, detail) {
  console.log(`NOTE  ${name}`);
  if (detail) console.log(`        ${detail}`);
}

const readU32 = (mem, addr) => new DataView(mem.buffer).getUint32(addr, true);
const readI32 = (mem, addr) => new DataView(mem.buffer).getInt32(addr, true);
const writeU32 = (mem, addr, v) =>
  new DataView(mem.buffer).setUint32(addr, v, true);
const writeI32 = (mem, addr, v) =>
  new DataView(mem.buffer).setInt32(addr, v, true);

// ------------------------------------------------------------ locate wasm-opt

function resolveWasmOpt() {
  const local = path.join(
    __dirname, '..', '.tools', 'node_modules', 'binaryen', 'bin', 'wasm-opt'
  );
  if (fs.existsSync(local)) {
    return { cmd: process.execPath, pre: [local], label: 'bundled binaryen' };
  }
  return { cmd: 'wasm-opt', pre: [], label: 'wasm-opt from PATH' };
}

function runWasmOpt(args) {
  const { cmd, pre, label } = resolveWasmOpt();
  try {
    execFileSync(cmd, [...pre, ...args], { stdio: 'pipe' });
  } catch (err) {
    console.error(
      `FAIL  could not run Binaryen (${label}).\n` +
        `        Install it with:\n` +
        `          npm install binaryen --prefix tools/fork-spike/.tools --no-save\n` +
        `        or point WASM_OPT at a native wasm-opt binary.\n` +
        `        ${String(err.message).split('\n')[0]}`
    );
    process.exit(2);
  }
}

console.log('== asyncify transform ==');
runWasmOpt(['--asyncify', baseWasm, '-o', asyncWasm]);
console.log(
  `transformed ${path.relative(process.cwd(), baseWasm)} -> ` +
    `${path.relative(process.cwd(), asyncWasm)} ` +
    `(${fs.statSync(baseWasm).size} -> ${fs.statSync(asyncWasm).size} bytes)\n`
);

const asyncModule = new WebAssembly.Module(fs.readFileSync(asyncWasm));
const exports_ = WebAssembly.Module.exports(asyncModule).map((e) => e.name);
for (const hook of [
  'asyncify_start_unwind',
  'asyncify_start_rewind',
  'asyncify_stop_rewind',
  'asyncify_get_state',
]) {
  check(`instrumented module exports ${hook}`, exports_.includes(hook));
}
console.log();

// --------------------------------------------------------------------- harness

// Each instance gets its own imports so `host_sendrec` closes over that
// instance's suspended/blocked state.
function makeImports(memory, st) {
  return {
    env: { memory },
    minix: {
      host_sendrec: (msgPtr) => {
        if (!st.blocked) {
          // The kernel knows this syscall cannot complete, so it drives the
          // unwind. It is the entity implementing the gate, so it has every
          // right to decide; the guest never learns.
          st.blocked = true;
          st.trace.push(['block']);
          st.inst.exports.asyncify_start_unwind(st.dataPtr);
          return 0; // ignored: the caller unwinds before using it
        }
        // Second pass, during rewind: the kernel has written its reply into
        // this process's message buffer and is now resuming it.
        const reply = readI32(memory, msgPtr + 12);
        st.trace.push(['resume', reply]);
        // The suspended operation has completed, so Asyncify must go back to
        // NORMAL before this import returns. The instrumented caller dispatches
        // on the state after the call: 2 replays the call site, 0 continues
        // forward, anything else is a trap. Emscripten calls this from the
        // guest's runtime wrapper; with a raw module the host's import is the
        // only place that knows the wait is over.
        st.inst.exports.asyncify_stop_rewind();
        return reply;
      },
      host_trace: (tag, value) => st.trace.push([tag, value]),
    },
  };
}

function newProc(opts = {}) {
  const bufSize = opts.bufSize ?? BUF_SIZE;
  const memory = new WebAssembly.Memory({ initial: MIN_PAGES, maximum: 1024 });
  const st = {
    blocked: false,
    trace: [],
    dataPtr: 0,
    bufStart: 0,
    bufEnd: 0,
    bufSize,
    inst: null,
  };
  const inst = new WebAssembly.Instance(asyncModule, makeImports(memory, st));
  st.inst = inst;

  // The asyncify_data struct lives in the process's own linear memory, which is
  // what makes it part of the snapshot. Layout read from the instrumented
  // hooks: +0 is the current stack pointer, +4 the buffer end (the hooks assert
  // stack_ptr <= end), +8 the buffer start.
  st.dataPtr = inst.exports.__heap_base.value;
  st.bufStart = st.dataPtr + STRUCT_SIZE;
  st.bufEnd = st.bufStart + bufSize;
  writeU32(memory, st.dataPtr + 0, st.bufStart);
  writeU32(memory, st.dataPtr + 4, st.bufEnd);
  writeU32(memory, st.dataPtr + 8, st.bufStart);

  return { memory, st, inst };
}

function resume(proc, reply, entry = 'async_process_main', arg = 0) {
  const { memory, st, inst } = proc;
  writeI32(memory, st.replySlot, reply);
  inst.exports.asyncify_start_rewind(st.dataPtr);
  const result = inst.exports[entry](arg);
  // host_sendrec calls asyncify_stop_rewind when it completes the wait, so a
  // normal return should already be back in NORMAL. Report it if it is not.
  const state = inst.exports.asyncify_get_state();
  if (state === STATE_REWINDING) inst.exports.asyncify_stop_rewind();
  return { result, state };
}

// Run a fresh process to its first suspension.
function suspend(proc) {
  const result = proc.inst.exports.async_process_main();
  return { result, state: proc.inst.exports.asyncify_get_state() };
}

// ------------------------------------------------- baseline: suspend + resume
//
// Before forking anything, establish that the protocol itself is right: a
// single process suspends at a blocking syscall and resumes with the reply.
// Without this step a fork failure is ambiguous between "the protocol is wrong"
// and "cloning is wrong".

console.log('-- baseline: suspend and resume, no fork --');
const solo = newProc();
solo.st.replySlot = solo.inst.exports.reply_slot_ptr();
const soloSuspended = suspend(solo);
check(
  'baseline process suspends at the blocking syscall',
  soloSuspended.result === 0 && soloSuspended.state === STATE_UNWINDING,
  `async_process_main() -> ${soloSuspended.result}, state -> ${soloSuspended.state}`
);
const soloRun = resume(solo, PARENT_REPLY);
check(
  'baseline process resumes with the reply',
  soloRun.result === PARENT_REPLY && soloRun.state === STATE_NORMAL,
  `async_process_main() -> ${soloRun.result}, state -> ${soloRun.state}`
);
check(
  'rewind resumes at the call site, not the entry point',
  solo.st.trace.filter((t) => t[0] === 1).length === 1,
  `trace = ${JSON.stringify(solo.st.trace)} (exactly one "send" entry)`
);
console.log();

// ------------------------------------------------------- boot, suspend, fork

console.log('-- fork: clone the suspended process --');

const parent = newProc();
parent.st.replySlot = parent.inst.exports.reply_slot_ptr();

const first = suspend(parent);
const stateAfterUnwind = first.state;
check(
  'process unwinds out of the blocking syscall',
  first.result === 0 && stateAfterUnwind === STATE_UNWINDING,
  `async_process_main() -> ${first.result}, asyncify_get_state() -> ${stateAfterUnwind} (1 = unwinding)`
);
console.log(`        parent trace: ${JSON.stringify(parent.st.trace)}`);

const stackPtrAtSuspend = readU32(parent.memory, parent.st.dataPtr);
check(
  'suspension state lives in the process memory',
  stackPtrAtSuspend > parent.st.bufStart && stackPtrAtSuspend <= parent.st.bufEnd,
  `asyncify_data.stack_ptr = ${stackPtrAtSuspend} inside buffer ` +
    `[${parent.st.bufStart}, ${parent.st.bufEnd}]`
);

// Snapshot, then clone. Copy AFTER instantiate(): instantiation re-applies data
// segments (layer 1 demonstrated the silent failure when you get this wrong).
const snapshot = new Uint8Array(parent.memory.buffer).slice();
const child = newProc();
child.st.replySlot = parent.st.replySlot;
new Uint8Array(child.memory.buffer).set(snapshot);
// Memory alone is not the whole process. The kernel's per-process bookkeeping —
// here, "this process is blocked on a syscall the kernel already owes a reply
// to" — lives host-side, like the `Proc` table does in the real kernel, so it
// has to be duplicated for the child. Get this wrong and the child re-blocks on
// the syscall it had already been waiting for.
child.st.blocked = true;

check(
  'child inherits the serialised stack',
  readU32(child.memory, child.st.dataPtr) === stackPtrAtSuspend,
  `child stack_ptr = ${readU32(child.memory, child.st.dataPtr)} ` +
    `(parent's = ${stackPtrAtSuspend})`
);

// Negative control: byte-for-byte the same clone, except the serialised stack
// buffer is zeroed. Everything else the child needs (data, the asyncify_data
// struct) is untouched, so if the serialised stack were not load-bearing this
// control would fork correctly too. Built before the parent resumes, since the
// parent's memory stops being a valid snapshot the moment it rewinds.
const broken = newProc();
broken.st.replySlot = parent.st.replySlot;
new Uint8Array(broken.memory.buffer).set(snapshot);
broken.st.blocked = true;
new Uint8Array(broken.memory.buffer).fill(0, broken.st.bufStart, broken.st.bufEnd);

// The global caveat: globals are not in linear memory, so the child starts with
// module-initial values. `asyncify_start_rewind` re-establishes both asyncify
// globals from the memory-resident struct, so the ones that matter are covered.
const parentSP = parent.inst.exports.__stack_pointer.value;
const childSPInitial = child.inst.exports.__stack_pointer.value;
check(
  '__stack_pointer is already correct in the child',
  parentSP === childSPInitial,
  `parent = ${parentSP}, child initial = ${childSPInitial} ` +
    `(a fully unwound stack returns to its base, so no copying is needed)`
);

// ---------------------------------------------------- resume both, divergently

const parentRun = resume(parent, PARENT_REPLY);
const childRun = resume(child, 0);

check(
  'parent rewinds and sees the child pid',
  parentRun.result === PARENT_REPLY,
  `async_process_main() -> ${parentRun.result}`
);
check(
  'child rewinds and sees 0',
  childRun.result === 0,
  `async_process_main() -> ${childRun.result}`
);

// The decisive evidence. If the rewind had failed and restarted the function
// from the top, the child would re-run host_trace(1, 0) and re-issue its fork
// request. It must appear exactly once, and only in the parent's history.
const parentSends = parent.st.trace.filter((t) => t[0] === 1).length;
const childSends = child.st.trace.filter((t) => t[0] === 1).length;
check(
  'child resumes mid-function, not from the entry point',
  childSends === 0 && parentSends === 1,
  `"send fork" entries: parent = ${parentSends}, child = ${childSends}`
);

check(
  'child does not re-issue the fork request',
  child.st.trace.every((t) => t[0] !== 'block'),
  `child trace = ${JSON.stringify(child.st.trace)}`
);

let brokenOutcome;
try {
  const r = resume(broken, 0);
  brokenOutcome = { trapped: false, result: r.result };
} catch (err) {
  brokenOutcome = { trapped: true, error: err.constructor.name };
}
check(
  'zeroing the serialised stack breaks the fork',
  brokenOutcome.trapped || brokenOutcome.result !== 0,
  brokenOutcome.trapped
    ? `control trapped (${brokenOutcome.error}) — the buffer is load-bearing`
    : `control returned ${brokenOutcome.result} instead of the correct 0`
);

console.log(`\nparent trace: ${JSON.stringify(parent.st.trace)}`);
console.log(`child  trace: ${JSON.stringify(child.st.trace)}`);
console.log(
  '        ("block" = syscall suspended by the host, "resume" = kernel replied, ' +
    '1/2 = guest trace tags)'
);

// ------------------------------------------------- deep stacks: sizing + cost
//
// Everything above suspends a one-frame-deep stack. A real fork() unwinds a
// userland stack many frames deep, and the buffer has to be sized for the worst
// case. These measure bytes-per-frame, confirm the fork still works at depth,
// and establish what happens when the buffer is too small.

console.log('\n-- deep stacks: buffer sizing and fork cost --');

const DEEP_ENTRY = 'async_deep_process_main';
const serialisedBytes = (proc) =>
  readU32(proc.memory, proc.st.dataPtr) - proc.st.bufStart;

const measurements = [];
for (const depth of [1, 16, 128, 1024]) {
  const p = newProc();
  p.st.replySlot = p.inst.exports.reply_slot_ptr();
  p.inst.exports[DEEP_ENTRY](depth);
  const suspendedState = p.inst.exports.asyncify_get_state();
  const bytes = serialisedBytes(p);
  measurements.push({ depth, bytes });

  const t0 = process.hrtime.bigint();
  const snap = new Uint8Array(p.memory.buffer).slice();
  const c = newProc();
  c.st.replySlot = p.st.replySlot;
  new Uint8Array(c.memory.buffer).set(snap);
  c.st.blocked = true;
  const forkUs = Number(process.hrtime.bigint() - t0) / 1000;

  const pr = resume(p, PARENT_REPLY, DEEP_ENTRY, depth);
  const cr = resume(c, 0, DEEP_ENTRY, depth);

  check(
    `fork at depth ${depth}`,
    suspendedState === STATE_UNWINDING &&
      pr.result === PARENT_REPLY &&
      cr.result === 0 &&
      c.st.trace.every((t) => t[0] !== 1),
    `${bytes} bytes serialised, snapshot+instantiate+copy ${forkUs.toFixed(0)}us, ` +
      `parent -> ${pr.result}, child -> ${cr.result}`
  );
}

// Bytes per frame, and what it implies for sizing a real process's buffer.
const deepest = measurements[measurements.length - 1];
const perFrame = deepest.bytes / deepest.depth;
note(
  'buffer sizing',
  `${perFrame.toFixed(1)} bytes per stack frame (from depth ${deepest.depth}); ` +
    `64 KiB covers ~${Math.floor(BUF_SIZE / perFrame)} frames, ` +
    `1 MiB ~${Math.floor(1048576 / perFrame)}, ` +
    `4 MiB ~${Math.floor(4194304 / perFrame)}`
);

// The buffer is finite, so the failure mode matters as much as the capacity.
// Asyncify's hooks only assert that the *initial* stack_ptr is <= the buffer
// end; the unwind path itself pushes frames without re-checking, so an
// undersized buffer may well write past the end rather than trapping. Probe it
// with a deliberately tiny buffer and a sentinel region behind it.
const GUARD = 8192;
const probe = newProc({ bufSize: 64 });
const guardStart = probe.st.bufEnd;
new Uint8Array(probe.memory.buffer).fill(0xaa, guardStart, guardStart + GUARD);
probe.st.replySlot = probe.inst.exports.reply_slot_ptr();

let probeOutcome;
try {
  probe.inst.exports[DEEP_ENTRY](512);
  probeOutcome = 'suspended without trapping';
} catch (err) {
  probeOutcome = `trapped (${err.constructor.name})`;
}
const probeView = new Uint8Array(probe.memory.buffer);
let clobbered = 0;
for (let i = guardStart; i < guardStart + GUARD; i++) {
  if (probeView[i] !== 0xaa) clobbered++;
}

if (probeOutcome.startsWith('trapped') || clobbered === 0) {
  note(
    'asyncify buffer overflow fails loudly',
    `depth 512 with a 64-byte buffer: ${probeOutcome}, ${clobbered} bytes of the ` +
      `sentinel region touched`
  );
} else {
  note(
    'asyncify buffer overflow is SILENT',
    `depth 512 with a 64-byte buffer: ${probeOutcome}, but ${clobbered} bytes past ` +
      `the buffer end were overwritten with no trap. The unwind path pushes ` +
      `frames without a bounds check, so the buffer must be sized for the ` +
      `deepest stack the process can reach — an overflow corrupts the ` +
      `process's own memory instead of failing.`
  );
}

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
