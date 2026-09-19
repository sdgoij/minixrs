'use strict';
//
// M1 boot harness: instantiate the wasm kernel instance with a host import
// boundary and check that it comes up and fails loudly.
//
// M1's exit criterion is that the kernel prints a banner through a host import
// and panics correctly, so both phases assert on what the *host* observed — the
// bytes that arrived through `host_console_write`, not anything the module
// claims about itself.

const fs = require('fs');
const path = require('path');

const wasmPath =
  process.argv[2] ||
  path.join(
    __dirname,
    '..',
    '..',
    'crates',
    'kernel-wasm',
    'target',
    'wasm32-unknown-unknown',
    'release',
    'kernel-wasm.wasm'
  );

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail) console.log(`        ${detail}`);
}
function note(name, detail) {
  console.log(`NOTE  ${name}`);
  if (detail) console.log(`        ${detail}`);
}

// -------------------------------------------------- simulated host devices

let consoleOut = '';
const consolePending = [];
let consoleWrites = 0;
let cycles = 0;
let haltCode = null;

const imports = {
  env: {
    host_console_write: (byte) => {
      consoleWrites += 1;
      consoleOut += String.fromCharCode(byte & 0xff);
    },
    host_console_read: () =>
      consolePending.length > 0 ? consolePending.shift() : -1,
    host_console_available: () => consolePending.length,
    // Monotonic and advancing on each read, so any kernel spin-wait on the
    // clock terminates instead of hanging the host.
    // The HAL declares this returning u64, so it must come back as a BigInt.
    host_cycles: () => BigInt((cycles += 1000)),
    host_halt: (code) => {
      haltCode = code;
    },
    // M1 has no processes, so there is nothing for a cross-process copy to
    // name. Refusing is the only honest answer here; M2 and the server harness
    // implement it, and this import exists only because the kernel build
    // references it unconditionally.
    host_copy_between: () => -14, // EFAULT
  },
};

// ------------------------------------------------------------- instantiate

const bytes = fs.readFileSync(wasmPath);
const wasmModule = new WebAssembly.Module(bytes);

console.log(
  `module:  ${path.relative(process.cwd(), wasmPath)} (${bytes.length} bytes)`
);
console.log(
  `imports: ${WebAssembly.Module.imports(wasmModule)
    .map((i) => `${i.module}.${i.name}`)
    .join(', ')}`
);
console.log(
  `exports: ${WebAssembly.Module.exports(wasmModule)
    .map((e) => e.name)
    .filter((n) => n.startsWith('minix_'))
    .join(', ')}\n`
);

const expectedImports = [
  'host_console_write',
  'host_console_read',
  'host_console_available',
  'host_cycles',
  'host_halt',
  'host_copy_between',
];
const importNames = WebAssembly.Module.imports(wasmModule).map((i) => i.name);
check(
  'the kernel reaches the host only through the declared import boundary',
  importNames.every((n) => expectedImports.includes(n)) &&
    importNames.includes('host_console_write'),
  `imports: ${importNames.join(', ')}`
);
// The boundary is not the declared set, it is the *used* set: --gc-sections
// drops imports nothing reaches, so an M1 build with no input path carries no
// input imports. Worth knowing before reading the import list as a contract.
const pruned = expectedImports.filter((n) => !importNames.includes(n));
if (pruned.length > 0) {
  note(
    'unused host imports are eliminated from the boundary',
    `declared but unreferenced in this build: ${pruned.join(', ')}`
  );
}

const instance = new WebAssembly.Instance(wasmModule, imports);

// ------------------------------------------------------- phase 1: bring up

console.log('-- phase 1: initialise --');
instance.exports.minix_kernel_init();
const banner = consoleOut;
check(
  'the kernel prints its banner through a host import',
  consoleWrites > 0 &&
    banner.includes('Hello MINIX!') &&
    banner.includes('wasm32 instance initialised'),
  `${consoleWrites} bytes arrived via host_console_write; captured ${JSON.stringify(banner)}`
);
const memorySize = instance.exports.memory
  ? instance.exports.memory.buffer.byteLength
  : 0;
console.log(`        kernel linear memory: ${memorySize} bytes\n`);

// ----------------------------------------------------- phase 2: fail loudly

console.log('-- phase 2: panic path --');
consoleOut = '';
haltCode = null;
let trapped = null;
try {
  instance.exports.minix_kernel_trigger_panic();
} catch (err) {
  trapped = err;
}
check(
  'a deliberate panic traps the instance',
  trapped instanceof WebAssembly.RuntimeError,
  trapped
    ? `trap: ${trapped.constructor.name}`
    : 'the call returned normally instead of trapping'
);
check(
  'the panic reports itself on the console before stopping',
  consoleOut.includes('deliberate M1 panic'),
  `captured ${JSON.stringify(consoleOut)}`
);
note(
  'terminal path',
  `host_halt was called with code ${haltCode}, then the instance trapped`
);

// ------------------------------------------------------------------ report

console.log(`\nconsole after the panic:\n${consoleOut.trimEnd()}\n`);
const failed = checks.filter((c) => !c.ok);
console.log(`${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
