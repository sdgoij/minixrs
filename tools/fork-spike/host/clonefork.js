'use strict';
//
// Fork spike, layer 1: the host-side clone-and-resume mechanics.
//
// Question: can a process suspended at a syscall be forked by snapshotting its
// linear memory and resuming that snapshot in a second instance, with the
// kernel writing a different syscall reply into each?
//
// This does NOT exercise Binaryen's Asyncify — see asyncify.js for that. The
// guest holds its continuation in linear memory by hand, which is the invariant
// Asyncify establishes for the real port. What this layer tests is everything
// the *host* has to get right, including three failure modes that are silent
// when you get them wrong:
//
//   1. Instantiation re-applies data segments, so copying the snapshot in
//      before instantiating loses it. A child that never receives the snapshot
//      restarts as a brand-new process — from the fork call, so it re-forks.
//   2. The child's memory must be the *same size* as the parent's, or a parent
//      that grew past the module minimum is truncated at the fork point.
//   3. Mutable wasm globals are not part of linear memory, so they are not
//      cloned. They must be re-exported and synchronised by the host.

const fs = require('fs');
const path = require('path');

const PAGE = 65536; // wasm linear memory granularity
const SUSPENDED = -1;
const MIN_PAGES = 64; // must be >= the guest's --initial-memory (4 MiB)
const PARENT_REPLY = 4242; // stands in for the child's pid

const wasmPath =
  process.argv[2] || path.join(__dirname, '..', 'build', 'guest.wasm');
const wasmBytes = fs.readFileSync(wasmPath);
const guestModule = new WebAssembly.Module(wasmBytes);

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail) console.log(`        ${detail}`);
}
function skip(name, why) {
  console.log(`SKIP  ${name}`);
  console.log(`        ${why}`);
}

const readU32 = (mem, addr) => new DataView(mem.buffer).getUint32(addr, true);
const readI32 = (mem, addr) => new DataView(mem.buffer).getInt32(addr, true);
const writeI32 = (mem, addr, v) =>
  new DataView(mem.buffer).setInt32(addr, v, true);

function newMemory(pages) {
  return new WebAssembly.Memory({ initial: pages, maximum: 1024 });
}

// Bind imports by discovering them, so renaming an import module in the guest
// does not silently break the harness.
function instantiate(memory, trace) {
  const imports = {};
  for (const imp of WebAssembly.Module.imports(guestModule)) {
    imports[imp.module] = imports[imp.module] || {};
    if (imp.kind === 'memory') {
      imports[imp.module][imp.name] = memory;
    } else if (imp.module === 'minix') {
      // A sendrec that cannot complete: the process blocks until the kernel
      // resumes it. Modelling PM's fork reply as always-blocking is the point.
      imports[imp.module].host_sendrec = () => SUSPENDED;
      imports[imp.module].host_trace = (tag, value) => trace.push([tag, value]);
    } else {
      throw new Error(`unexpected import ${imp.module}.${imp.name}`);
    }
  }
  return new WebAssembly.Instance(guestModule, imports);
}

function newProcess(pages) {
  const memory = newMemory(pages);
  const trace = [];
  const instance = instantiate(memory, trace);
  return { instance, memory, trace };
}

console.log(
  `module:  ${path.relative(process.cwd(), wasmPath)} (${wasmBytes.length} bytes)`
);
console.log(
  `imports: ${WebAssembly.Module.imports(guestModule)
    .map((i) => `${i.module}.${i.name}:${i.kind}`)
    .join(', ')}`
);
console.log(
  `exports: ${WebAssembly.Module.exports(guestModule)
    .map((e) => `${e.name}:${e.kind}`)
    .join(', ')}\n`
);

// ------------------------------------------------------------- boot + suspend

const parent = newProcess(MIN_PAGES);
console.log(
  `memory:  ${parent.memory.buffer.byteLength} bytes = ${MIN_PAGES} wasm pages of ${PAGE}`
);
console.log(`         (1 wasm page = 16 MINIX 4 KiB pages)\n`);

const first = parent.instance.exports.process_main();
check(
  'process suspends at the fork sendrec',
  first === SUSPENDED,
  `process_main() -> ${first}`
);

const resumePc = parent.instance.exports.resume_pc_ptr();
const msgPtr = parent.instance.exports.msg_ptr();
const replySlot = parent.instance.exports.reply_slot_ptr();

check(
  'continuation is memory-resident after suspension',
  readU32(parent.memory, resumePc) === 1,
  `RESUME_PC@0x${resumePc.toString(16)} = ${readU32(parent.memory, resumePc)} (1 = "after the fork call")`
);
console.log(
  `        message 0x${msgPtr.toString(16)}: [${[0, 1, 2, 3]
    .map((i) => readI32(parent.memory, msgPtr + i * 4))
    .join(', ')}]  <- type, dst, PM_FORK, reply`
);

// --------------------------------------------------- snapshot + copy ordering

const pages = parent.memory.buffer.byteLength / PAGE;
// slice() copies: memory.buffer is a live view and would otherwise alias the
// parent, and it detaches if either side grows.
const snapshot = new Uint8Array(parent.memory.buffer).slice();

const fresh = newProcess(pages);
const freshPc = readU32(fresh.memory, resumePc);
check(
  'a fresh instance does NOT inherit the parent state',
  freshPc === 0,
  `fresh child RESUME_PC = ${freshPc} (0 = "entry"), so the snapshot must be copied in AFTER instantiate()`
);

// Show the consequence rather than only asserting it: a child that never got
// the snapshot runs from the top and issues its own fork request.
fresh.instance.exports.process_main();
check(
  'an unsnapshotted child restarts and re-issues fork',
  fresh.trace.length === 1 && fresh.trace[0][0] === 1,
  `trace = ${JSON.stringify(fresh.trace)} (tag 1 = "sending the fork request")`
);

// ---------------------------------------------------------------- the fork

const child = newProcess(pages);
new Uint8Array(child.memory.buffer).set(snapshot); // AFTER instantiate()

check(
  'child memory is byte-identical to the parent snapshot',
  Buffer.compare(
    Buffer.from(child.memory.buffer),
    Buffer.from(parent.memory.buffer)
  ) === 0,
  'after the copy, before either side resumes'
);
check(
  'child inherits the continuation',
  readU32(child.memory, resumePc) === 1,
  `child RESUME_PC = ${readU32(child.memory, resumePc)}`
);

// Mutable globals are not in linear memory, so they are not cloned. Report the
// gap and demonstrate the mitigation: an exported global is host-writable.
const parentGlobal = parent.instance.exports.__stack_pointer;
if (parentGlobal === undefined) {
  skip(
    'host can synchronise an exported mutable global',
    '__stack_pointer was not exported; add -C link-arg=--export=__stack_pointer'
  );
} else {
  const parentSP = parentGlobal.value;
  const childBefore = child.instance.exports.__stack_pointer.value;
  child.instance.exports.__stack_pointer.value = parentSP;
  const childAfter = child.instance.exports.__stack_pointer.value;
  check(
    'host can synchronise an exported mutable global',
    childAfter === parentSP,
    `__stack_pointer parent=${parentSP} fresh child=${childBefore} -> set to ${childAfter}`
  );
}

// The kernel writes a different reply into each instance before resuming. This
// is the entire mechanism by which parent and child diverge.
writeI32(parent.memory, replySlot, PARENT_REPLY);
writeI32(child.memory, replySlot, 0);

const parentResult = parent.instance.exports.process_main();
const childResult = child.instance.exports.process_main();

check(
  'parent resumes and sees the child pid',
  parentResult === PARENT_REPLY,
  `process_main() -> ${parentResult}`
);
check(
  'child resumes and sees 0',
  childResult === 0,
  `process_main() -> ${childResult}`
);

// -------------------------------------------------------------- independence

const markerAddr = msgPtr + 7 * 4;
writeI32(parent.memory, markerAddr, 0x5eed);
check(
  'the two instances have independent memory',
  readI32(child.memory, markerAddr) !== 0x5eed,
  `parent wrote 0x5eed at 0x${markerAddr.toString(16)}; child reads ${readI32(child.memory, markerAddr)}`
);

console.log(`\nparent trace: ${JSON.stringify(parent.trace)}`);
console.log(`child  trace: ${JSON.stringify(child.trace)}`);
console.log('        (1 = sending fork, 2 = resuming with the reply)');

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
