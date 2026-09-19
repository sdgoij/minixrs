'use strict';
//
// Dump a wasm module's host boundary and memory shape. Used while bringing the
// real servers up, where the interesting questions are which imports the linker
// actually left (it prunes unused ones) and how much of the layout the module
// claims for itself.
//
//   node tools/wasm-servers/inspect.cjs <module.wasm>
//
// Locals avoid `module` and `exports`: those are CommonJS bindings, and
// redeclaring either makes Node's syntax detection misread the file.

const fs = require('fs');

const wasmModule = new WebAssembly.Module(fs.readFileSync(process.argv[2]));

const importList = WebAssembly.Module.imports(wasmModule);
console.log(`imports (${importList.length}):`);
for (const i of importList) console.log(`  ${i.module}.${i.name}`);

const exportList = WebAssembly.Module.exports(wasmModule);
console.log(`exports (${exportList.length}):`);
for (const e of exportList) console.log(`  ${e.kind} ${e.name}`);
