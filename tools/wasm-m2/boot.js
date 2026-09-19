'use strict';
//
// M2 host loop: instance-per-process plus the dispatch protocol (§4.1/§4.2).
//
// The question M2 has to answer is whether the design's crux works with the
// *kernel's own* scheduler in charge — the fork spike proved a suspended process
// can be cloned and resumed, but the host decided everything there. Here the
// kernel owns the process table and run queues, and the host only asks it three
// questions per step: who should run, what did the syscall do, and is that
// process now waiting.
//
// The Asyncify protocol is the one measured in the spike: the host starts the
// unwind from inside the syscall import, and must return the state to NORMAL
// when the wait ends or the resumed caller traps.

const fs = require('fs');
const path = require('path');

const buildDir = path.join(__dirname, 'build');
const kernelPath = path.join(buildDir, 'kernel.wasm');
const procPath = path.join(buildDir, 'procs.async.wasm');

// asyncify_data is {stack_ptr, end, start} at +0/+4/+8 with an ascending buffer.
const STRUCT_SIZE = 16;
const BUF_SIZE = 65536;

const SEND = 0x01;
const RECEIVE = 0x02;
const STATE_UNWINDING = 1;

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  // Details are diagnostics for failures; printing them on success would put
  // failure wording next to a PASS. Passing evidence goes in the timeline dump.
  if (!ok && detail) console.log(`        ${detail}`);
}
function note(name, detail) {
  console.log(`NOTE  ${name}`);
  if (detail) console.log(`        ${detail}`);
}

const readU32 = (mem, addr) => new DataView(mem.buffer).getUint32(addr, true);
const writeU32 = (mem, addr, v) =>
  new DataView(mem.buffer).setUint32(addr, v, true);

// --------------------------------------------------------------- console

// One log for the whole machine, tagged by whoever is running, so the *ordering*
// across processes is the evidence rather than three separate transcripts.
let timeline = [];
let currentLine = '';
let consoleTag = 'kernel';

function emit(byte) {
  const ch = String.fromCharCode(byte & 0xff);
  if (ch === '\n') {
    timeline.push(`${consoleTag}: ${currentLine}`);
    currentLine = '';
    return;
  }
  if (ch !== '\r') currentLine += ch;
}

let cycles = 0;
let haltCode = null;

// ------------------------------------------------------- kernel instance

const kernelModule = new WebAssembly.Module(fs.readFileSync(kernelPath));
const kernel = new WebAssembly.Instance(kernelModule, {
  env: {
    host_console_write: (b) => {
      consoleTag = 'kernel';
      emit(b);
    },
    host_console_read: () => -1,
    host_console_available: () => 0,
    host_cycles: () => (cycles += 1000),
    host_halt: (code) => {
      haltCode = code;
    },
  },
});

kernel.exports.minix_kernel_init();

// ------------------------------------------------------ process instances

const procModule = new WebAssembly.Module(fs.readFileSync(procPath));

const specs = [
  { slot: 100, peerSlot: 101, entry: 'proc_a', label: 'A' },
  { slot: 101, peerSlot: 100, entry: 'proc_b', label: 'B' },
];

// Ask the kernel for the endpoints instead of inventing them. An endpoint is the
// kernel's own generation+slot encoding and IPC resolves a destination by
// decoding it, so a hand-picked number finds nothing — the first attempt at this
// milestone deadlocked exactly that way.
for (const s of specs) {
  s.endpoint = kernel.exports.minix_make_endpoint(s.slot);
}
for (const s of specs) {
  s.peerEndpoint = specs.find((p) => p.slot === s.peerSlot).endpoint;
}

// The payload the kernel structurally cannot move: `mini_send` has no way to
// read another instance's memory, so the host carries it.
let carriedPayload = null;

function makeProc(spec) {
  const memory = new WebAssembly.Memory({ initial: 64, maximum: 1024 });
  const st = {
    spec,
    memory,
    inst: null,
    started: false,
    exited: false,
    pending: null,
    blockedCount: 0,
    dataPtr: 0,
  };

  const imports = {
    env: {
      memory,
      host_console_write: (b) => {
        consoleTag = spec.label;
        emit(b);
      },
      host_syscall: (nr, dst) => {
        if (st.pending !== null) {
          // Resumed: the kernel has answered, so put Asyncify back to NORMAL
          // before returning or the instrumented caller re-enters its rewind
          // path and traps.
          st.inst.exports.asyncify_stop_rewind();
          const value = st.pending;
          st.pending = null;
          return value;
        }

        // A real kernel would deliver the message as part of completing the
        // receive. Here the host is the layer that can see both memories, so it
        // does the copy at the delivery point.
        if (nr === RECEIVE && carriedPayload) {
          const addr = st.inst.exports.msg_ptr();
          new Uint8Array(st.memory.buffer).set(carriedPayload, addr);
          carriedPayload = null;
        }

        const result = kernel.exports.minix_syscall(spec.slot, nr, dst);

        if (kernel.exports.minix_proc_blocked(spec.slot) === 1) {
          st.pending = result;
          st.blockedCount += 1;
          if (nr === SEND) {
            const addr = st.inst.exports.msg_ptr();
            carriedPayload = new Uint8Array(st.memory.buffer).slice(addr, addr + 64);
          }
          st.inst.exports.asyncify_start_unwind(st.dataPtr);
          return 0; // ignored: the caller unwinds before using it
        }
        return result;
      },
    },
  };

  st.inst = new WebAssembly.Instance(procModule, imports);
  st.dataPtr = st.inst.exports.__heap_base.value;
  const bufStart = st.dataPtr + STRUCT_SIZE;
  writeU32(memory, st.dataPtr + 0, bufStart);
  writeU32(memory, st.dataPtr + 4, bufStart + BUF_SIZE);
  writeU32(memory, st.dataPtr + 8, bufStart);
  return st;
}

const procs = specs.map(makeProc);

const spawnFailures = specs.filter(
  (s) => kernel.exports.minix_proc_spawn(s.slot, s.endpoint) !== 0
);
check(
  'the kernel accepts both processes into its table',
  spawnFailures.length === 0,
  spawnFailures.map((s) => s.label).join(', ')
);

// ---------------------------------------------------------- dispatch loop

function run(st) {
  if (st.pending !== null) {
    st.inst.exports.asyncify_start_rewind(st.dataPtr);
  } else {
    if (!st.started) {
      // The kernel hands a process its identity at exec; the host writes it here
      // for the same reason it carries payloads — only the host can see this
      // instance's memory.
      const info = st.inst.exports.info_ptr();
      const view = new DataView(st.memory.buffer);
      view.setUint32(info + 0, st.spec.endpoint, true);
      view.setUint32(info + 4, st.spec.peerEndpoint, true);
    }
    st.started = true;
  }
  const result = st.inst.exports[st.spec.entry]();
  const state = st.inst.exports.asyncify_get_state();
  if (state === STATE_UNWINDING) {
    // It blocked inside the syscall; the kernel is holding it.
    return;
  }
  st.exited = true;
  kernel.exports.minix_proc_exit(st.spec.slot);
}

let steps = 0;
let converged = true;
for (;;) {
  const slot = kernel.exports.minix_step();
  if (slot === -1) break;
  if (++steps > 32) {
    converged = false;
    break;
  }
  const st = procs.find((p) => p.spec.slot === slot);
  if (st === undefined) {
    converged = false;
    note('the kernel picked a slot the host never spawned', `slot ${slot}`);
    break;
  }
  run(st);
}

check('the dispatch loop converged', converged, `${steps} steps`);

// ------------------------------------------------------------- assertions

const expected = [
  'A: sending to B',
  'B: waiting for A',
  "B: got A's payload",
  'A: unblocked',
];
const sequence = timeline.filter((line) => /^[AB]: /.test(line));
check(
  'the processes interleave exactly as the rendezvous requires',
  JSON.stringify(sequence.map((l) => l.replace(/^[AB]: /, ''))) ===
    JSON.stringify(expected),
  `observed:\n          ${sequence.join('\n          ')}`
);

const procA = procs[0];
check(
  'A blocked in the kernel rather than spinning',
  procA.blockedCount === 1,
  `A blocked ${procA.blockedCount} time(s)`
);
check(
  'A was resumed after B completed the rendezvous',
  procA.started && procA.exited,
  `started=${procA.started} exited=${procA.exited}`
);
check(
  'the payload crossed instances through the host',
  sequence.some((l) => l.includes("got A's payload")),
  'B never reported receiving the payload'
);
check(
  'the kernel run queues are consistent afterwards',
  kernel.exports.minix_runqueues_ok() === 1,
  'runqueues_ok() reported an inconsistency'
);
note(
  'what the kernel and host each contributed',
  `the kernel blocked and later re-queued A (${procA.blockedCount} block), decided the ` +
    'run order, and completed the rendezvous; the host carried the payload bytes, ' +
    'which no part of the kernel could have moved.'
);

console.log('\ntimeline:');
for (const line of timeline) console.log(`  ${line}`);

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
