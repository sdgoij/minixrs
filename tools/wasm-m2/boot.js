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

const RECEIVE = 47;
const BRK = 36;
const STATE_UNWINDING = 1;

// Where the kernel's brk window ends: `user_heap_base()` + the 1 MiB it
// accepts. Nothing but the host can turn that window into memory, so it is
// grown to cover it before a process first runs — the analogue of the kernel's
// exec-time pre-map on the arches that have a page table.
const HEAP_WINDOW_END = 0x0030_0000;
const PAGE = 65536;

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

// ------------------------------------------------------- cross-process copy
//
// The host is this port's page table. Two instances' address spaces are two
// linear memories and the host owns both, so the kernel's own copies — the IPC
// payload, the message delivery, and SYS_VIRCOPY — come here. The kernel still
// decides *what* is copied; this only moves the bytes, and refuses what it
// cannot reach rather than trapping on it.

/// The memory a copy endpoint names: the kernel's own for a negative process
/// number, else the instance spawned for that slot.
function memoryFor(proc) {
  if (proc < 0) return kernel.exports.memory;
  const st = procs.find((p) => p.spec.slot === proc);
  return st === undefined ? null : st.memory;
}

const EFAULT = -14;

/// Every cross-process copy the kernel asked for, so a failure says *which* copy
/// went wrong rather than only that the bytes did not arrive.
const copyLog = [];

function copyBetween(srcProc, srcAddr, dstProc, dstAddr, bytes) {
  const src = memoryFor(srcProc);
  const dst = memoryFor(dstProc);
  // An unknown process, or a range past the end of an instance, is an address
  // this port cannot reach — which is exactly what EFAULT means, and preferable
  // to a trap that would take the whole machine down.
  let result = 0;
  if (src === null || dst === null) result = EFAULT;
  else if (srcAddr < 0 || dstAddr < 0 || bytes < 0) result = EFAULT;
  else if (srcAddr + bytes > src.buffer.byteLength) result = EFAULT;
  else if (dstAddr + bytes > dst.buffer.byteLength) result = EFAULT;
  else {
    new Uint8Array(dst.buffer).set(new Uint8Array(src.buffer, srcAddr, bytes), dstAddr);
  }
  copyLog.push({ srcProc, srcAddr, dstProc, dstAddr, bytes, result });
  return result;
}

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
    // The HAL declares this returning u64, so it must come back as a BigInt.
    host_cycles: () => BigInt((cycles += 1000)),
    host_halt: (code) => {
      haltCode = code;
    },
    host_copy_between: copyBetween,
  },
});

kernel.exports.minix_kernel_init();

// ------------------------------------------------------ process instances

const procModule = new WebAssembly.Module(fs.readFileSync(procPath));

const specs = [
  { slot: 100, peerSlot: 101, entry: 'proc_a', label: 'A' },
  { slot: 101, peerSlot: 100, entry: 'proc_b', label: 'B' },
  { slot: 102, entry: 'proc_heap', label: 'H' },
  // The SYS_VIRCOPY pair, labelled V/W so they do not collide with the guest
  // entry points' own names.
  { slot: 103, peerSlot: 104, entry: 'proc_copy_src', label: 'V' },
  { slot: 104, peerSlot: 103, entry: 'proc_copy_dst', label: 'W' },
];

// Ask the kernel for the endpoints instead of inventing them. An endpoint is the
// kernel's own generation+slot encoding and IPC resolves a destination by
// decoding it, so a hand-picked number finds nothing — the first attempt at this
// milestone deadlocked exactly that way.
for (const s of specs) {
  s.endpoint = kernel.exports.minix_make_endpoint(s.slot);
}
for (const s of specs) {
  s.peerEndpoint = specs.find((p) => p.slot === s.peerSlot)?.endpoint ?? 0;
}

// The kernel's own copies now do move bytes on this port — `copy_from_user`
// pulls the sender's message into the kernel and `delivermsg` pushes it into the
// receiver — so the host no longer hand-carries payloads between instances. It
// supplies the copy primitive instead, which is the arrangement §5.1 predicts:
// the kernel decides what moves, the host is only able to reach both memories.

function makeProc(spec) {
  const memory = new WebAssembly.Memory({ initial: 32, maximum: 1024 });
  const st = {
    spec,
    memory,
    inst: null,
    started: false,
    exited: false,
    pending: null,
    blockedCount: 0,
    dataPtr: 0,
    memoryBefore: memory.buffer.byteLength,
    grants: 0,
  };

  // The pager. `memory.grow` is host-side, so every byte a process can address
  // beyond its image comes from here — either because exec mapped the heap
  // window up front, or because the kernel granted a break past it.
  st.backTo = (end) => {
    const have = st.memory.buffer.byteLength;
    if (end <= have) return;
    st.memory.grow(Math.ceil((end - have) / PAGE));
    st.grants += 1;
  };

  const imports = {
    env: {
      memory,
      host_console_write: (b) => {
        consoleTag = spec.label;
        emit(b);
      },
      minix_syscall: (nrRaw, a0, a1) => {
        // Seven parameters at the ABI; only the first three are named here
        // because this path needs no more. wasm i64 arrives as a BigInt, and
        // addresses fit comfortably in a Number, so normalise up front:
        // comparing a Number against a BigInt is silently false.
        const nr = Number(nrRaw);
        const dst = Number(a0);
        const msgAddr = Number(a1);
        if (st.pending !== null) {
          // Resumed: the kernel has answered. The message is already in this
          // instance's memory, put there by the kernel's `delivermsg`.
          st.pending = null;
          // Put Asyncify back to NORMAL before returning, or the instrumented
          // caller re-enters its rewind path and traps.
          st.inst.exports.asyncify_stop_rewind();
          // Not the value cached at block time: this is the epilogue a hardware
          // arch reaches on the way back to userland, and a receive satisfied
          // while the instance was unwound gets its answer there (`mini_send`
          // stores the sender's endpoint in the receiver's frame).
          return BigInt(kernel.exports.minix_proc_retval(spec.slot));
        }

        // The kernel's own ABI is (i32 slot, i64 nr, u64 x6), so the integer
        // width has to be right on the way back in.
        const result = kernel.exports.minix_syscall(
          spec.slot,
          BigInt(nr),
          BigInt(dst),
          BigInt(msgAddr),
          0n,
          0n,
          0n,
          0n
        );
        const blocked = kernel.exports.minix_proc_blocked(spec.slot) === 1;

        // A granted break has to be backed by memory before the process can use
        // it, and this is the only layer that can add any. The kernel has
        // already decided whether the break is legal, so the host is not
        // choosing a policy here, only enacting one.
        if (nr === BRK && result > 0n) st.backTo(Number(result));

        if (blocked) {
          st.blockedCount += 1;
          st.pending = { value: result, nr, msgAddr };
          st.inst.exports.asyncify_start_unwind(st.dataPtr);
          // BigInt, because the import's return type is i64 and Node will not
          // coerce a Number across that boundary.
          return 0n;
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
      // exec's pre-map: the kernel has already accepted breaks anywhere in its
      // window, so the memory behind them has to exist before the process runs.
      st.backTo(HEAP_WINDOW_END);
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
  'A: getpid routed',
  'A: sending to B',
  'B: waiting for A',
  'B: payload intact',
  'B: m_source agrees with the syscall result',
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
  'the gate routes the whole syscall table, not just IPC',
  sequence.some((l) => l.includes('getpid routed')),
  'getpid did not dispatch — the syscall table is not wired'
);
// Scoped to the A/B rendezvous: the log also carries the heap process's kernel
// calls and the V/W copy, so counting all of it would assert the wrong thing.
const abCopies = copyLog.filter((c) => c.srcProc === 100 || c.dstProc === 101);
check(
  "the payload crossed instances through the kernel's own copy path",
  sequence.some((l) => l.includes('payload intact')),
  'B did not report an intact payload'
);
check(
  'the kernel asked for exactly the two copies that rendezvous needs',
  abCopies.length === 2 &&
    abCopies.every((c) => c.result === 0) &&
    abCopies[0].dstProc === -1 &&
    abCopies[1].srcProc === -1,
  abCopies.map((c) => `${c.srcProc}->${c.dstProc}=${c.result}`).join(', ')
);
check(
  "the kernel's m_source agrees with the syscall result",
  sequence.some((l) => l.includes('m_source agrees')),
  'the host-written m_source and the kernel return value disagreed'
);
check(
  'the kernel run queues are consistent afterwards',
  kernel.exports.minix_runqueues_ok() === 1,
  'runqueues_ok() reported an inconsistency'
);

// -------------------------------------------------------------- heap (brk)

const procC = procs.find((p) => p.spec.label === 'H');
const reportPtr = procC.inst.exports.heap_report_ptr();
const reportView = new DataView(procC.memory.buffer);
const heap = [0, 1, 2, 3].map((i) => reportView.getBigUint64(reportPtr + i * 8, true));

check(
  'the initial break is the HAL heap base, not a hardcoded arch address',
  heap[0] === 0x00200000n,
  `brk(0) returned 0x${heap[0].toString(16)}`
);
check(
  'the kernel grants a break past its pre-mapped window',
  heap[1] === 0x00218000n,
  `brk(0x218000) returned 0x${heap[1].toString(16)}`
);
check(
  'the host had to grow the instance before the heap could be used',
  procC.memoryBefore < HEAP_WINDOW_END && procC.memory.buffer.byteLength >= HEAP_WINDOW_END,
  `memory was ${procC.memoryBefore} bytes at instantiation and ${procC.memory.buffer.byteLength} after the first run`
);
check(
  'the granted heap byte round-trips',
  heap[3] === 0x5an,
  `read back 0x${heap[3].toString(16)}`
);
check(
  'a break outside the window is refused rather than silently accepted',
  timeline.some((l) => l.endsWith('out-of-window brk refused')),
  'the kernel did not answer ENOMEM to an out-of-window break'
);
note(
  'who owned what in the heap path',
  `the kernel decided whether each break was legal (the window is derived from the HAL, not a fixed ` +
    `address); the host was the pager, growing the instance ${procC.grants} time(s) — the heap window ` +
    'at exec, and any break the kernel later grants past it; the process touched only memory the ' +
    'kernel had granted.'
);
// ----------------------------------------------------- SYS_VIRCOPY (call 15)

// The one kernel operation DS needs before it can read a client's key, and the
// only place the seam is exercised process-to-process rather than
// kernel-to-process. V asks the kernel to copy its buffer into W's memory; W
// then looks at *its own* memory, so the assertion cannot be satisfied by
// anything the host or the sender did locally.

const procV = procs.find((p) => p.spec.label === 'V');
const procW = procs.find((p) => p.spec.label === 'W');

// Both instances are the same module, so both have a `COPY_REPORT` static and
// each writes only its own slots: V issues the copy, W looks at the result. Read
// from whoever wrote the field, or the check passes on an untouched zero.
function reportOf(p) {
  const view = new DataView(p.memory.buffer);
  const ptr = p.inst.exports.copy_report_ptr();
  return [0, 1, 2, 3].map((i) => view.getBigUint64(ptr + i * 8, true));
}
const fromV = reportOf(procV);
const fromW = reportOf(procW);

check(
  'SYS_VIRCOPY was accepted by the kernel',
  fromV[1] === 0n,
  `kernel call 15 returned ${fromV[1]}`
);
check(
  "the copied bytes are visible in the receiver's own memory",
  fromW[0] === 0x0c0ffee0n,
  `W read 0x${fromW[0].toString(16)} at the copy destination`
);
check(
  'the sender and the receiver are different instances',
  procV.memory !== procW.memory && fromW[2] !== 0n,
  'the copy could have been satisfied within one memory'
);
check(
  "the kernel's own vircopy was the thing that moved the bytes",
  copyLog.some((c) => c.srcProc === 103 && c.dstProc === 104 && c.result === 0),
  copyLog.map((c) => `${c.srcProc}->${c.dstProc}`).join(', ')
);

note(
  'the divisions that had to be put back',
  'Three arch-shape assumptions only a port without page tables and without a ' +
    'syscall-return path exposes. `virtual_copy` switched CR3, `copy_from_user` ' +
    'and `delivermsg` addressed the other process directly, and the receive path ' +
    'left DELIVERMSG for an epilogue written in asm. None is reachable from the ' +
    'host, so each became a named seam rather than a special case — and ' +
    'SYS_VIRCOPY, which DS needs, needed two of the three.'
);

note(
  'what the kernel and host each contributed',
  `the kernel blocked and later re-queued A (${procA.blockedCount} block), decided the run ` +
    'order, and completed the rendezvous. It also moved the payload itself: ' +
    '`copy_from_user` pulled it into the kernel and `delivermsg` pushed it into B. ' +
    'The host supplied one primitive — a copy between two memories it owns — and the ' +
    'kernel decided what to copy and when.'
);

console.log('\nSYS_VIRCOPY (call 15) report:');
console.log(`  V: vircopy return = ${fromV[1]}`);
console.log(`  W: byte read from its own memory = 0x${fromW[0].toString(16)}`);
console.log(`  W: receive returned ${fromW[2]}`);

// ------------------------------------------------------- remaining seam sites
//
// The kernel-call handlers that take an address out of a message and
// dereference it. Each is a cross-address-space transfer, so on this port each
// is a place where a direct access would read or write the *kernel's* own
// memory instead of the process's. The assertion is the copy the kernel asked
// for, which is the only thing separating "went through the seam" from
// "happened to land somewhere harmless".
//
// Driven as kernel calls (50) straight from the host: the handlers take a
// message and nothing else, so a guest program would be a longer route to the
// same bytes.

const KERNEL_CALL_NR = 50;
const SYS_EXEC = 1;
const SYS_MEMSET = 13;
const SYS_VUMAP = 18;
const ENDPOINT_SELF = 31742;

const seamSpec = {
  slot: 105,
  label: 'S',
  // Never called: these handlers need a message, not a running process. The
  // spec only exists so `makeProc` builds an instance and `memoryFor` can find
  // it by slot.
  entry: 'proc_unused',
  peerSlot: undefined,
  peerEndpoint: 0,
};
seamSpec.endpoint = kernel.exports.minix_make_endpoint(seamSpec.slot);
const seam = makeProc(seamSpec);
procs.push(seam);
kernel.exports.minix_proc_spawn(seamSpec.slot, seamSpec.endpoint);

// Write a message into an instance's own message buffer and dispatch a kernel
// call with it. Returns the kernel's answer and every copy it asked for while
// answering, which is what the checks below read. `slot` names the caller, which
// is the process the kernel takes its notion of "the calling process" from — the
// grant check below needs the granter to be a process other than the caller.
function kernelCall(slot, callNr, writePayload) {
  const st = procs.find((p) => p.spec.slot === slot);
  const msg = st.inst.exports.msg_ptr();
  new Uint8Array(st.memory.buffer, msg, 64).fill(0);
  writePayload(new DataView(st.memory.buffer), msg);
  const before = copyLog.length;
  const result = kernel.exports.minix_syscall(
    slot,
    BigInt(KERNEL_CALL_NR),
    BigInt(callNr),
    BigInt(msg),
    0n,
    0n,
    0n,
    0n
  );
  return { result: Number(result), copies: copyLog.slice(before) };
}

// SYS_MEMSET (call 13) — `do_memset_handler` filling a process's memory.
//
// The handler resolves the message's `process` field and fills that instance.
// The bytes have to appear in the *process's* memory and the kernel has to have
// asked for a copy to put them there; a direct write satisfies neither, because
// the address is an offset into the kernel's own image there. Payload offsets
// are written as literals: they start at 8, past the call-number/source header
// `sys_kernel_call_handler` writes, which is itself part of what is under test.
const memsetBuf = seam.inst.exports.copy_buf_ptr();
new Uint8Array(seam.memory.buffer, memsetBuf, 32).fill(0);
const memset = kernelCall(seamSpec.slot, SYS_MEMSET, (view, msg) => {
  view.setBigUint64(msg + 8, BigInt(memsetBuf), true); // base
  view.setBigUint64(msg + 16, 32n, true); // count
  view.setBigUint64(msg + 24, 0xabn, true); // pattern
  view.setInt32(msg + 32, seamSpec.endpoint, true); // process
});
const memsetBytes = new Uint8Array(seam.memory.buffer, memsetBuf, 32);
check(
  'SYS_MEMSET was accepted by the kernel',
  memset.result === 0,
  `kernel call 13 returned ${memset.result}`
);
check(
  "SYS_MEMSET's pattern arrived in the process's own memory",
  memsetBytes.every((b) => b === 0xab),
  `read ${[...memsetBytes.slice(0, 8)].join(',')}...`
);
check(
  "the fill crossed instances through the kernel's copy seam",
  memset.copies.some(
    (c) =>
      c.srcProc === -1 &&
      c.dstProc === seamSpec.slot &&
      c.dstAddr === memsetBuf &&
      c.bytes === 32
  ),
  memset.copies.map((c) => `${c.srcProc}->${c.dstProc}:${c.bytes}`).join(', ')
);

// SYS_EXEC (call 1) — `do_exec_handler` reading the program name.
//
// The name lives in the caller's address space, so the read is a copy out of
// that instance into a kernel stack buffer. `ps_str` needs no read: it is only
// ever passed to `arch_proc_init` as a value.
const nameAddr = seam.inst.exports.copy_buf_ptr() + 32;
const name = 'seamtest';
new Uint8Array(seam.memory.buffer, nameAddr, name.length + 1).set(
  [...name].map((ch) => ch.charCodeAt(0))
);
const exec = kernelCall(seamSpec.slot, SYS_EXEC, (view, msg) => {
  view.setInt32(msg + 8, seamSpec.endpoint, true); // endpt — the caller itself
  view.setBigUint64(msg + 16, 0x1000n, true); // ip
  view.setBigUint64(msg + 24, 0x2000n, true); // stack
  view.setBigUint64(msg + 32, BigInt(nameAddr), true); // name
  view.setBigUint64(msg + 40, 0n, true); // ps_str
});
check(
  "SYS_EXEC read the program name out of the caller's instance",
  exec.copies.some(
    (c) => c.srcProc === seamSpec.slot && c.srcAddr === nameAddr && c.bytes === 15
  ),
  exec.copies
    .map((c) => `${c.srcProc}:0x${c.srcAddr.toString(16)}->${c.dstProc}:${c.bytes}`)
    .join(', ')
);

// SYS_VUMAP (call 18) — `do_vumap_handler` reading the caller's vector.
//
// The syscall answers EFAULT and cannot do otherwise here: its output is
// physical addresses and this port has none, because `vm_lookup_range` walks a
// page table that does not exist. The *read of the vector* happens before that,
// though, and it is the cross-address-space transfer under test — so the check
// is the kernel's request rather than the syscall's result, and the note below
// says which part is proven and which part is a recorded limitation.
const vumapVec = seam.inst.exports.copy_buf_ptr() + 16;
const vumapOut = seam.inst.exports.copy_buf_ptr() + 48;
{
  const v = new DataView(seam.memory.buffer);
  v.setBigUint64(vumapVec + 0, BigInt(vumapVec), true); // vv_u.u_addr
  v.setUint32(vumapVec + 8, 16, true); // vv_size
}
const vumap = kernelCall(seamSpec.slot, SYS_VUMAP, (view, msg) => {
  view.setInt32(msg + 8, ENDPOINT_SELF, true); // endpt — local addresses
  view.setBigUint64(msg + 16, BigInt(vumapVec), true); // vaddr
  view.setInt32(msg + 24, 1, true); // vcount
  view.setBigUint64(msg + 32, 0n, true); // offset
  view.setInt32(msg + 40, 1, true); // access = VUA_READ
  view.setBigUint64(msg + 48, BigInt(vumapOut), true); // paddr
  view.setInt32(msg + 56, 8, true); // pmax
});
check(
  "SYS_VUMAP read the caller's vector through the seam",
  vumap.copies.some(
    (c) => c.srcProc === seamSpec.slot && c.srcAddr === vumapVec && c.bytes === 16
  ),
  vumap.copies
    .map((c) => `${c.srcProc}:0x${c.srcAddr.toString(16)}->${c.dstProc}:${c.bytes}`)
    .join(', ')
);

// ------------------------------------------- SYS_SETGRANT (34) + SYS_SAFECOPYFROM (31)
//
// `verify_grant` resolves a grant id by reading the *granter's* grant table, and
// that table lives in the granter's memory — out of the granter's own address
// space, not out of a message field, which is why the message-shaped audit that
// found the other sites missed it. On this port the kernel cannot reach another
// instance's memory any more than it can a page table, so the read belongs on the
// same seam as everything else; a handler that dereferenced the address would read
// the kernel's own image at that offset and refuse every grant as unauthorised.
//
// The pair below is the shape DS needs for its label table: a process publishes a
// table (SYS_SETGRANT) and another copies out of a grant it describes
// (SYS_SAFECOPYFROM). The granter is A's instance, which has already run and is
// blocked in a send, and `SYS_SETGRANT` is what gives it a privilege structure —
// its `p_priv` is null until this point, which is exactly what the handler
// allocates one for.
const SYS_SETGRANT = 34;
const SYS_SAFECOPYFROM = 31;

const granterSpec = specs.find((s) => s.slot === 100);
const granterState = procs.find((p) => p.spec.slot === granterSpec.slot);
const granterScratch = granterState.inst.exports.grant_scratch_ptr();
const granteeScratch = seam.inst.exports.grant_scratch_ptr();

// One `CpGrant` and the eight bytes it covers. The offsets are `arch-common`'s
// `test_cp_grant_layout` — cp_flags @0, `cp_direct.cp_who_to` @8, `cp_start` @16,
// `cp_len` @24 (`usize`, so four bytes on this target), and 48 bytes for the
// table. The flags are CPF_USED|CPF_VALID|CPF_DIRECT|CPF_READ.
const GRANT_SIZE = 48;
const GRANT_PAYLOAD_AT = 48;
const GRANT_PAYLOAD_WORD = 0x0c0ffee0;

new Uint8Array(granterState.memory.buffer).fill(0, granterScratch, granterScratch + 64);
{
  const view = new DataView(granterState.memory.buffer);
  view.setInt32(granterScratch + 0, 0x001301, true); // cp_flags
  view.setInt32(granterScratch + 8, seamSpec.endpoint, true); // cp_who_to — the grantee
  view.setBigUint64(
    granterScratch + 16,
    BigInt(granterScratch + GRANT_PAYLOAD_AT),
    true
  ); // cp_start
  view.setUint32(granterScratch + 24, 8, true); // cp_len
  view.setUint32(granterScratch + GRANT_PAYLOAD_AT, GRANT_PAYLOAD_WORD, true);
}

const setgrant = kernelCall(granterSpec.slot, SYS_SETGRANT, (view, msg) => {
  view.setBigUint64(msg + 8, BigInt(granterScratch), true); // addr
  view.setInt32(msg + 16, 1, true); // entries
});
check(
  'the granter registered a one-entry grant table with the kernel',
  setgrant.result === 0,
  `kernel call 34 returned ${setgrant.result}`
);

const copiedTo = granteeScratch + GRANT_PAYLOAD_AT;
const safecopy = kernelCall(seamSpec.slot, SYS_SAFECOPYFROM, (view, msg) => {
  view.setInt32(msg + 8, granterSpec.endpoint, true); // granter
  view.setInt32(msg + 12, 0, true); // grant id
  view.setBigUint64(msg + 16, 0n, true); // offset into the grant
  view.setBigUint64(msg + 24, BigInt(copiedTo), true); // caller's destination
  view.setBigUint64(msg + 32, 8n, true); // bytes
});
check(
  'SYS_SAFECOPYFROM resolved the grant and copied out of it',
  safecopy.result === 0,
  `kernel call 31 returned ${safecopy.result}`
);
check(
  "the kernel read the grant entry out of the granter's own instance",
  safecopy.copies.some(
    (c) =>
      c.srcProc === granterSpec.slot &&
      c.srcAddr === granterScratch &&
      c.dstProc === -1 &&
      c.bytes === GRANT_SIZE
  ),
  safecopy.copies
    .map((c) => `${c.srcProc}:0x${c.srcAddr.toString(16)}->${c.dstProc}:${c.bytes}`)
    .join(', ')
);
check(
  'the granted bytes crossed from the granter to the caller',
  safecopy.copies.some(
    (c) => c.srcProc === granterSpec.slot && c.dstProc === seamSpec.slot && c.bytes === 8
  ) && readU32(seam.memory, copiedTo) === GRANT_PAYLOAD_WORD,
  `read back 0x${readU32(seam.memory, copiedTo).toString(16)}`
);

// ------------------------------------------- SYS_SAFEMEMSET (56)
//
// The other direction, and the only handler that asks the kernel to *write* into
// the granter through the seam: `do_safememset` verifies a CPF_WRITE grant and
// hands the pattern to the copy HAL (`vm_memset` stages it in a kernel buffer and
// gives each chunk to the seam), so the bytes can only land if the kernel asks the
// host to put them there. The flags live in the granter's own table — granter
// memory — so granting write is a write to that table, which is also how a real
// granter would do it.
const SYS_SAFEMEMSET = 56;
const SAFE_FILL_PATTERN = 0x5a;

{
  const view = new DataView(granterState.memory.buffer);
  // CPF_USED|CPF_VALID|CPF_DIRECT|CPF_READ|CPF_WRITE.
  view.setInt32(granterScratch + 0, 0x001303, true);
}
const safememset = kernelCall(seamSpec.slot, SYS_SAFEMEMSET, (view, msg) => {
  view.setInt32(msg + 8, granterSpec.endpoint, true); // granter
  view.setInt32(msg + 12, 0, true); // grant id
  view.setBigUint64(msg + 16, 0n, true); // offset into the grant
  view.setBigUint64(msg + 24, BigInt(SAFE_FILL_PATTERN), true); // pattern
  view.setBigUint64(msg + 32, 8n, true); // bytes
});
check(
  'SYS_SAFEMEMSET verified the write grant and returned OK',
  safememset.result === 0,
  `kernel call 56 returned ${safememset.result}`
);
check(
  "the pattern landed in the granter's own instance",
  readU32(granterState.memory, granterScratch + GRANT_PAYLOAD_AT) === 0x5a5a5a5a,
  `read back 0x${readU32(granterState.memory, granterScratch + GRANT_PAYLOAD_AT).toString(16)}`
);
check(
  'the fill crossed the seam into the granter',
  safememset.copies.some((c) => c.dstProc === granterSpec.slot && c.bytes > 0),
  safememset.copies.map((c) => `${c.srcProc}->${c.dstProc}:${c.bytes}`).join(', ')
);

note(
  'what the remaining seams establish, and what they do not',
  'SYS_MEMSET, SYS_EXEC, SYS_VUMAP and the SYS_SETGRANT/SYS_SAFECOPYFROM pair ' +
    'move bytes through the same seam SYS_VIRCOPY uses, and each check is the ' +
    'kernel asking for the copy rather than the effect alone — so a handler that ' +
    "read or wrote the kernel's own memory would fail even if the bytes happened " +
    'to appear correct. The grant pair is the one whose address did not come out ' +
    `of a message at all. SYS_VUMAP answered ${vumap.result} because its result ` +
    'is physical addresses and this port has none: the vector read is the whole ' +
    'of what can be exercised until a physical-address model is decided, and ' +
    '`PORTING_PLAN.md` finding 14 records that rather than papering over it. ' +
    'SYS_SAFEMEMSET is the write half of the same grant, so the pair above and ' +
    'this one cover both directions of `verify_grant`.'
);

// Printed last, after every check has run: the timeline and the copy log are
// whole-run artefacts, and a copy a later handler asks for must appear in them.
console.log('\ntimeline:');
for (const line of timeline) console.log(`  ${line}`);

console.log(`\ncross-process copies the kernel asked for (${copyLog.length}):`);
for (const c of copyLog) {
  console.log(
    `  ${c.srcProc}:0x${c.srcAddr.toString(16)} -> ${c.dstProc}:0x${c.dstAddr.toString(16)}` +
      ` (${c.bytes} bytes) => ${c.result}`
  );
}

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
