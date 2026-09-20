'use strict';
//
// Real servers under the kernel's own scheduler: DS, RS and PM.
//
// The question this answers is narrower than "does the boot chain work" and
// deliberately so: can a *real* server, compiled for the real target, be
// instantiated as a process and driven as far as its main loop? Each of the
// three runs its own init and then `loop { RECEIVE }`, so the first syscall an
// instance issues is its first receive — which is the whole assertion. Nothing
// is asked of the servers, and nothing is added to them to make it checkable.
//
// What is *not* claimed here: that they can talk to each other — which is what
// the DS client at the bottom of this file changes. It publishes a value to DS
// and reads it back through `minix_util`'s real client, so DS answers a real
// request for the first time, and it can only answer it by copying the key out
// of the client's instance (the host mediates that, §5.1).

const fs = require('fs');
const path = require('path');

const buildDir = path.join(__dirname, 'build');
const kernelPath = path.join(buildDir, 'kernel.wasm');
const serverPath = path.join(buildDir, 'servers.async.wasm');
// A program as its own module (`wasm-program`), Asyncify'd for the same reason the servers
// are: the harness's dispatch loop reads Asyncify's state after every entry, so a module that
// was not instrumented has no `asyncify_get_state` to ask.
const programPath = path.join(buildDir, 'program.async.wasm');
// The boot filesystem image, in the same place the image builder writes it. For wasm32 that image
// carries the *program module* rather than ELF executables (see `run.sh`), so it is both the root
// filesystem and — since step 4 — where an exec's bytes come from.
const ramdiskImagePath = path.join(
  __dirname,
  '..',
  '..',
  'target',
  'images',
  'wasm32-minix',
  'minixfs.img'
);
// `RAMDISK_IMAGE_VA` on wasm32: exactly `MAX_USER_ADDRESS`, so the window is
// outside the process's own VA range. See `arch_common::com`.
const RAMDISK_IMAGE_VA = 0x1000000;
const WASM_PAGE = 65536;

const RECEIVE = 47;
const SENDREC = 48;
const SEND = 46;
const SENDNB = 51;
const ANY = 0x0000ffff;
/// `SYS_EXIT`. `minix-rt::exit` issues this and then traps, so it is also how a
/// trap is told apart from a failure.
const EXIT = 0;
/// Kernel call 7 is `SYS_GETKSIG`, which PM asks for on every notification.
const KERNEL_CALL = 50;
const GETKSIG = 7;
/// Kernel call 26 is `SYS_GETINFO`, which is how the client learns its own
/// endpoint before announcing itself to RS.
const SYS_GETINFO = 26;
/// `GET_IMAGE`, one of `SYS_GETINFO`'s requests: the kernel's image table. PM asks for it before
/// it receives anything, which is where its process table comes from (`init_boot_procs`).
const GET_IMAGE = 1;
const STATE_UNWINDING = 1;
const STRUCT_SIZE = 16;
const BUF_SIZE = 65536;
const EINVAL = -22;

// A runaway process cannot be preempted on this port (§6.4): if a guest loops
// inside one dispatch, the host never gets control back and the run cannot even
// report what happened. The budget is the host's only lever. Once it is spent
// the gate answers EINVAL, which is enough to break a spin, and the trace stops
// growing — both matter because the first version of this harness recorded every
// syscall and exhausted Node's heap instead of diagnosing anything.
//
// It is a budget for the whole run, not per instance, so it has to cover what the
// *script* costs as well as the boot chain: every console byte the shell reads is
// a read plus its echo write, each of those is a VFS round trip to the tty, and a
// fork clones an instance and an exec creates one. 2000 was enough while the shell
// could only run a builtin; a shell that forks, execs and reaps needs several
// times that, and the run then ended with `vfs spent the 2000-syscall budget` in
// the middle of the script rather than at the end.
const SYSCALL_BUDGET = 20000;
const TRACE_LIMIT = 64;
// How many of the *latest* syscalls to keep. Small on purpose: the loop checks
// want the last one and the few before it.
const TAIL_LIMIT = 8;
let syscallsLeft = SYSCALL_BUDGET;
/// Which instance ran the budget out, if any. Named rather than booleans so the
/// failure report says who was spinning.
let exhaustedBy = null;

/// Thrown by the syscall gate once the budget is spent, instead of answering an error.
///
/// An error *return* is something the guest decides what to do with, and a guest in a
/// retry loop may simply retry: the tty's blocking read treats `EINVAL` as "the console is
/// gone" and gives up, which is at least an ending, but a reader that treats it as any
/// other failure would loop again. A throw unwinds out of the guest's dispatch to the
/// loop that owns it, which can then stop and say who was spinning. Finding 32 is the run
/// that needed this: the loop was inside a single dispatch, so neither the step cap nor a
/// return value could end it, and the run had to be killed from outside.
class BudgetExhausted extends Error {}

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (!ok && detail) console.log(`        ${detail}`);
}
function note(name, detail) {
  console.log(`NOTE  ${name}`);
  if (detail) console.log(`        ${detail}`);
}

const readU32 = (mem, addr) => new DataView(mem.buffer).getUint32(addr, true);
const writeU32 = (mem, addr, v) =>
  new DataView(mem.buffer).setUint32(addr, v, true);

let timeline = [];
let currentLine = '';
let consoleTag = 'kernel';

function emit(byte) {
  const ch = String.fromCharCode(byte & 0xff);
  if (ch === '\n') {
    timeline.push(`${consoleTag}: ${currentLine}`);
    // Print it as it arrives when tracing. The timeline is only dumped when a run ends,
    // and "the run never ends" is exactly when a guest's own output is the only evidence
    // there is (finding 32).
    if (process.env.WASM_TRACE === '1') console.log(`${consoleTag}: ${currentLine}`);
    currentLine = '';
    return;
  }
  if (ch !== '\r') currentLine += ch;
}

let cycles = 0;
let haltCode = null;

// ------------------------------------------------------- cross-process copy
//
// The host is this port's page table: it owns every instance's memory, so the
// kernel's own copies — IPC payloads, message delivery, and SYS_VIRCOPY, which
// is what DS uses to read a client's key — arrive here. The kernel decides what
// moves; this only moves the bytes, and refuses what it cannot reach rather
// than trapping on it.

const EFAULT = -14;
const copyLog = [];

/// The memory a copy endpoint names: the kernel's own for a negative process
/// number, else the instance spawned for that slot.
function memoryFor(proc) {
  if (proc < 0) return kernel.exports.memory;
  const st = procs.find((p) => p.spec.slot === proc);
  return st === undefined ? null : st.memory;
}

function copyBetween(srcProc, srcAddr, dstProc, dstAddr, bytes) {
  const src = memoryFor(srcProc);
  const dst = memoryFor(dstProc);
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

// ------------------------------------------------------------------ exec
//
// Exec on this port is module instantiation (§7.2): the kernel has no image to install, so it
// names the process, the bytes and the arguments, and the host creates the instance. Since
// §7.2's step 4 those bytes are the *executable* — the file VFS read off the boot image — which
// is why the host compiles them here rather than looking anything up: the image is the module
// store, and an executable that is not a module this engine can run is a failure the engine
// reports itself.
//
// The addresses in the request are in two different memories, and the kernel's comment on
// `ExecModuleRequest` says which is which: the image is in the calling process's, while the path
// and the arguments are in the kernel's. Only the host owns both (§5.1), which is the whole
// reason the request is addresses rather than bytes.

const ENOEXEC = -8;
const ENOMEM = -12;

/// Every fork the kernel asked for: who, into which slot, and when in the parent's syscall
/// history. Recorded from the *kernel's* request, so the checks read what happened rather than
/// what the harness intended.
const forks = [];

/// Field offsets of `ExecModuleRequest` — which asserts its own layout on the Rust side.
const EXEC_REQ_IMAGE_PROC = 0;
const EXEC_REQ_IMAGE_ADDR = 4;
const EXEC_REQ_IMAGE_LEN = 8;
const EXEC_REQ_PATH_ADDR = 12;
const EXEC_REQ_ARGV_ADDR = 16;
const EXEC_REQ_ARGC = 20;

/// Read a NUL-terminated string from an instance's memory, bounded: the string comes from a
/// process's address space, so the scan has to end somewhere even when the terminator does not.
function cString(mem, addr, max) {
  const bytes = new Uint8Array(mem.buffer);
  let end = addr;
  const limit = Math.min(addr + max, bytes.length);
  while (end < limit && bytes[end] !== 0) end += 1;
  return Buffer.from(bytes.subarray(addr, end)).toString('utf8');
}

/// The host's half of exec: compile the module the kernel named and instantiate it as the
/// process in `slot`, with the arguments it supplied.
///
/// Returns 0, or a negative errno. `ENOEXEC` is the interesting one and it is deliberately one
/// answer to several causes — bytes that are not a module, a module whose imports this port does
/// not supply, a module with no entry this port knows how to call — because they all mean the
/// same thing to the caller: this executable is not a program that can run here.
function hostExecModule(slot, requestAddr) {
  const kernelMemory = kernel.exports.memory;
  const req = new DataView(kernelMemory.buffer);
  const imageProc = req.getInt32(requestAddr + EXEC_REQ_IMAGE_PROC, true);
  const imageAddr = req.getUint32(requestAddr + EXEC_REQ_IMAGE_ADDR, true);
  const imageLen = req.getUint32(requestAddr + EXEC_REQ_IMAGE_LEN, true);
  const pathAddr = req.getUint32(requestAddr + EXEC_REQ_PATH_ADDR, true);
  const argvAddr = req.getUint32(requestAddr + EXEC_REQ_ARGV_ADDR, true);
  const argc = req.getUint32(requestAddr + EXEC_REQ_ARGC, true);

  const imageMemory = memoryFor(imageProc);
  if (imageMemory === null) {
    note('the exec request names a process the host does not have', `proc=${imageProc}`);
    return EINVAL;
  }
  const st = procs.find((p) => p.spec.slot === slot);
  if (st === undefined || st.inst === null) return EINVAL;

  // Where and how big, without a copy: the bytes stay in the process that read them, and both
  // checks are the engine's — a range past the end of the memory throws here.
  const image = new Uint8Array(imageMemory.buffer, imageAddr, imageLen);
  const path = cString(imageMemory, pathAddr, 256);
  const argv = [];
  let at = argvAddr;
  for (let i = 0; i < argc; i += 1) {
    const arg = cString(kernelMemory, at, 4096);
    argv.push(arg);
    at += Buffer.byteLength(arg, 'utf8') + 1;
  }

  let module;
  try {
    // The compile *is* the validation: nothing else about a wasm module can be checked without
    // doing this, which is why the kernel hands over bytes it has not looked at.
    module = new WebAssembly.Module(image);
  } catch (e) {
    note(
      'the executable is not a wasm module this port can run',
      `path=${JSON.stringify(path)} (${imageLen} bytes): ${e.message}`
    );
    return ENOEXEC;
  }

  // A program module exports one entry, by convention: `minix_program_main(argc, argv)`. The
  // path chose the module and `argv[0]` chooses the program inside it, so the entry does not
  // have to be looked up by name — but it does have to be there, and saying so beats
  // instantiating a module whose entry the harness would then call and fail to find.
  const entry = 'minix_program_main';
  if (!WebAssembly.Module.exports(module).some((e) => e.name === entry && e.kind === 'function')) {
    note('the module has no program entry', `path=${JSON.stringify(path)}: expected ${entry}`);
    return ENOEXEC;
  }

  try {
    // The old instance and its memory are kept: the checks read the reports a replaced image left
    // behind in them (`init_report_ptr`'s three values), and "the old instance is gone" is a
    // claim about which instance the *slot* runs, not about the object.
    st.prev = { inst: st.inst, memory: st.memory, entry: st.entry };
    instantiate(st, module, argv, entry);
    st.exited = false;
    st.exec = {
      path,
      argv,
      moduleBytes: imageLen,
      // Where in the console transcript the swap happened, so a check can distinguish a line
      // the *new* image wrote from one the old image wrote before it was replaced. The
      // abandoned image cannot write anything after this point: its blocked syscall was
      // dropped with `st.pending` and the host never re-enters it.
      timelineAt: timeline.length,
      atSyscall: st.syscalls,
    };
    return 0;
  } catch (e) {
    note('instantiating the exec target module failed', `${path}: ${e}`);
    return ENOMEM;
  }
}

// ----------------------------------------------------------- kernel instance

const kernelModule = new WebAssembly.Module(fs.readFileSync(kernelPath));
// What the shell reads when it asks for a line. The kernel drains these bytes into its
// own serial ring (`poll_console`) and the tty pulls them from there, so this is the
// port's console input seen from outside. The script is `exit`-terminated on purpose: an
// empty read answers EAGAIN and the shell's editor retries in user mode, so a shell left
// without input spins with syscalls and the budget is the only thing that would end it.
//
// The first line is 5b's subject and the second is M3f's: `echo` is the shell's own builtin,
// while `/bin/echo` is a path, so the shell forks, the child execs the module the image carries
// at that path, and the shell reaps it — `fork` and `exec` in one line of a script.
//
// Each command is named once, and both the script and the checks that look at its output are
// built from it. A literal in the script and another in the check is how a check keeps passing
// against a command nobody runs any more.
const EXTERNAL_CMD = '/bin/echo';
const EXTERNAL_ARGS = ['hi'];
const BUILTIN_CMD = 'echo';
const BUILTIN_ARGS = ['second'];
const consoleInput = Array.from(
  Buffer.from(
    `${[EXTERNAL_CMD, ...EXTERNAL_ARGS].join(' ')}\n` +
      `${[BUILTIN_CMD, ...BUILTIN_ARGS].join(' ')}\nexit\n`
  )
);

const kernel = new WebAssembly.Instance(kernelModule, {
  env: {
    host_console_write: (b) => {
      consoleTag = 'kernel';
      emit(b);
    },
    host_console_read: () => {
      if (consoleInput.length === 0) return -1;
      const b = consoleInput.shift();
      if (process.env.WASM_TRACE === '1')
        console.log(`  [input] host supplied 0x${b.toString(16)} (${consoleInput.length} left)`);
      return b;
    },
    host_console_available: () => consoleInput.length,
    host_cycles: () => BigInt((cycles += 1000)),
    host_halt: (code) => {
      haltCode = code;
    },
    host_copy_between: copyBetween,
    // The kernel's exec arm asks for the new image here (§7.2); see `hostExecModule`.
    host_exec_module: hostExecModule,
    // ...and its fork arm asks for the child's memory (§12 risk 1); see `hostForkProcess`.
    host_fork_process: hostForkProcess,
  },
});

kernel.exports.minix_kernel_init();

// ---------------------------------------------------------- server instances

const serverModule = new WebAssembly.Module(fs.readFileSync(serverPath));
const programModule = new WebAssembly.Module(fs.readFileSync(programPath));

// The program's arguments, as the host supplies them. The module dispatches on argv[0] and
// `echo` prints the rest, so this array is the thing the console line below is compared
// against — one array rather than a literal in the spec and another in the check.
const PROGRAM_ARGV = ['echo', 'hello', 'from', 'a', 'module'];

// Boot order, and the process numbers `BOOT_IMAGE` gives them: DS is 6, RS is 2,
// PM is 0. With generation 0 an endpoint equals its process number, which is why
// `minix_make_endpoint` answers the same value — but it is still asked rather
// than assumed, because a hand-picked endpoint is what deadlocked M2. DS's 6 is
// also what `minix_util`'s client hardcodes as `DS_ENDPOINT`, which is why the
// client can use the real library rather than a hand-rolled message.
const specs = [
  { slot: 6, entry: 'minix_server_ds', label: 'ds' },
  { slot: 2, entry: 'minix_server_rs', label: 'rs' },
  { slot: 0, entry: 'minix_server_pm', label: 'pm' },
  // The RAM disk block driver, at `RAMDISK_PROC_NR`. It owns device 0, which is
  // the boot filesystem image the host copies into this instance below — the wasm
  // equivalent of the kernel mapping the image on the hardware arches.
  { slot: 11, entry: 'minix_server_ramdisk', label: 'ramdisk' },
  // VM before MFS: every process that calls `brk()` depends on it, and MFS's
  // allocator runs during init.
  { slot: 8, entry: 'minix_server_vm', label: 'vm' },
  { slot: 7, entry: 'minix_server_mfs', label: 'mfs' },
  // Spawned with no device attached, on purpose: `mount_root` prefers the
  // `virtio_blk` driver and asks it whether it has a device, and an instance that
  // is absent would leave that probe blocked on a peer that does not exist. It
  // answers `EIO`, which is true here, and the ramdisk fallback fires.
  { slot: 12, entry: 'minix_server_virtio_blk', label: 'virtio_blk' },
  // VFS's init mounts devman's tree right after the root filesystem, and
  // `mount_devman` blocks until it starts.
  { slot: 15, entry: 'minix_server_devman', label: 'devman' },
  // VFS last: its init calls `mount_root`, which asks MFS for the root superblock,
  // so MFS has to be alive and answering first.
  { slot: 1, entry: 'minix_server_vfs', label: 'vfs' },
  // The console's driver, spawned once VFS has mounted devman: its init registers the
  // console with devman (`devman_add_device("tty0", 0)`) and must retry while that
  // tree is not up yet, so running it after VFS is what makes the first attempt the
  // one that lands. It is the process `/dev/console` (major 5) has to resolve to.
  { slot: 5, entry: 'minix_server_tty', label: 'tty' },
  // INIT — the first *user* process, at `INIT_PROC_NR`. The one instance here that
  // is not a server: the kernel links it to the shared USER privilege slot, and its
  // lifetime ends. Spawned with the boot processes, after the servers it talks to.
  { slot: 10, entry: 'minix_init', label: 'init' },
  // Not a boot process and not a server: the client that gives DS something to
  // answer. Its slot is above the boot procs on purpose, so `p_priv` stays null
  // — `may_send_to` allows anything from a privilege-less process, and allows DS
  // to reply to one, which is what lets a plain client reach a system server
  // whose `s_ipc_to` mask was never filled in (nothing calls `fill_sendto_mask`).
  { slot: 20, entry: 'minix_ds_client', label: 'client' },
  // The control: the same client, never announced to RS, so DS has no label for
  // it. Its publish must still be refused.
  { slot: 21, entry: 'minix_ds_client_unregistered', label: 'unregistered' },
  // A program as its own module (M7a step 1). Not a server and not a boot process: its slot
  // is outside `BOOT_IMAGE`, so the kernel has no privilege structure to attach and the host
  // asks for the shared USER slot explicitly (`spawnAsUser`) — the same link `do_fork` makes
  // for a new user process. `argv` is written into the module's own argv area before the
  // entry runs; the entry takes it as a C argv, so the host passes the count and a pointer.
  //
  // `startAfterBoot`: the kernel must not have it in the run queue during boot. A spawned slot
  // is immediately runnable, so a boot-time program is scheduled at the first opportunity —
  // which here is while INIT is blocked on its `getpid` reply, where it wrote into the middle
  // of INIT's console line. The module's instance is created with the others; only the spawn
  // waits.
  {
    slot: 22,
    entry: 'minix_program_main',
    label: 'program',
    argv: PROGRAM_ARGV,
    spawnAsUser: true,
    startAfterBoot: true,
    module: programModule,
  },
  // M7b step 5a's subject: the same module, asked for by a different name, so the program that
  // runs is the one that forks.
  //
  // Slot 23, outside the kernel's `BOOT_IMAGE` — a process the kernel has and the boot image does
  // not name, which is the case the boot arches cannot have (PM creates every process there). What
  // lets PM serve it is the image table: this harness is the loader on this port (§7), so it
  // declares the slot to the kernel before the boot chain starts (`minix_image_add`, below), PM
  // registers every entry the kernel names, and the slot is one PM will not hand to a child. Slot
  // 3 was where this program used to sit, borrowing `memory`'s endpoint because PM's knowledge of
  // the boot processes was a hardcoded 0..10 range and that was the only way in: finding 39.
  {
    slot: 23,
    entry: 'minix_program_main',
    label: 'forktest',
    argv: ['forktest'],
    spawnAsUser: true,
    startAfterBoot: true,
    module: programModule,
  },
];

for (const s of specs) s.endpoint = kernel.exports.minix_make_endpoint(s.slot);

// The host is this port's loader, so what it will run is what the kernel's image table has to
// name. PM reads that table when it starts (finding 39's fix), and takes from it both which
// processes exist and which slots are not free — so a slot declared here is registered and its
// slot is kept out of the free list a `fork` child would otherwise be given.
//
// The `startAfterBoot` specs are declared here, at boot, and spawned later: a boot image names its
// processes before any of them run, and that ordering is exactly what a slot spawned early would
// break (it would be scheduled mid-boot, splitting INIT's console line — see `spawnInstance`).
const declared = specs.filter(
  (s) => kernel.exports.minix_image_add(s.slot, s.endpoint) !== 0
);
check(
  'the kernel accepts every declared slot into its image table',
  declared.length === 0,
  declared.map((s) => s.label).join(', ')
);

/// Write argv into the instance's argv area, in the layout the module declares: `argc`, then
/// a pointer array, then the NUL-terminated strings.
///
/// The offsets are the module's, not the host's — the area's address comes from
/// `argv_area_ptr()`, which is the only way a static inside a wasm module can be named. What
/// the host writes is the harness's own `spec.argv`, and what the program prints is what it
/// read back, so the two ends are compared rather than the host being asserted against
/// itself.
function writeArgv(st, argv) {
  const base = st.inst.exports.argv_area_ptr();
  const view = new DataView(st.memory.buffer);
  view.setUint32(base, argv.length, true);
  let at = base + 4 + 4 * argv.length;
  argv.forEach((arg, i) => {
    view.setUint32(base + 4 + 4 * i, at, true);
    const bytes = Buffer.from(arg, 'utf8');
    new Uint8Array(st.memory.buffer, at, bytes.length).set(bytes);
    // Wasm memory starts zeroed and nothing has written here, so the byte after each string
    // is already the terminator the entry scans for.
    at += bytes.length + 1;
  });
  return base;
}

/// Build the import object an instance of this slot gets.
///
/// A function of the *slot* rather than of the instance, because exec replaces a slot's
/// instance and the replacement has to talk to the kernel through the same gate, in the same
/// slot's name. `imports.env.memory` is read here, so `st.memory` must already be the memory
/// the new instance is to own.
function makeImports(st) {
  const memory = st.memory;
  const spec = st.spec;

  return {
    env: {
      memory,
      host_cycles: () => BigInt((cycles += 1000)),
      // The servers reach this through `drivers`' wasm HAL boundary, which is
      // the same one the kernel uses. Nothing has asked for a copy yet, but DS
      // will: `SYS_VIRCOPY` is how it reads a client's key, so the primitive is
      // the real one rather than a refusal.
      host_copy_between: copyBetween,
      // All six argument registers are named and forwarded, not only the two the
      // message-passing syscalls use. The kernel's dispatcher hands `args` straight
      // to the handler, and the three-argument syscalls read `args[2]` — `write`'s
      // count among them. Naming only `a0`/`a1` replaced the rest with a literal
      // zero, so a console `write` transferred `count = 0` bytes and reported
      // success: a panicking server's message reached the kernel and went nowhere
      // (finding 29).
      minix_syscall: (nrRaw, a0, a1, a2, a3, a4, a5) => {
        const nr = Number(nrRaw);
        const dst = Number(a0);
        const msgAddr = Number(a1);
        if (syscallsLeft <= 0) {
          exhaustedBy = spec.label;
          throw new BudgetExhausted(`${spec.label} spent the ${SYSCALL_BUDGET}-syscall budget`);
        }
        syscallsLeft -= 1;
        if (st.trace.length < TRACE_LIMIT) st.trace.push({ nr, a0: dst });
        st.tail.push({ nr, a0: dst });
        if (st.tail.length > TAIL_LIMIT) st.tail.shift();
        st.syscalls += 1;

        if (st.pending !== null) {
          // Resumed: the kernel has answered, and the message is already in this
          // instance's memory — put there by the kernel's own delivery, which
          // goes through the HAL copy seam. The host does not carry it.
          st.pending = null;
          // Put Asyncify back to NORMAL before returning, or the instrumented
          // caller re-enters its rewind path and traps.
          st.inst.exports.asyncify_stop_rewind();
          // Deliberately not the value cached at block time: a receive that
          // blocked is satisfied later, and `mini_send` stores the *sender's*
          // endpoint in the receiver's frame return slot. On a hardware arch the
          // syscall-return epilogue restores it; a wasm instance has no such
          // path, so the host reads it out. The cached value was OK — at block
          // time no sender was known.
          const rv = kernel.exports.minix_proc_retval(spec.slot);
          return BigInt(rv);
        }

        const result = kernel.exports.minix_syscall(
          spec.slot,
          BigInt(nr),
          BigInt(a0),
          BigInt(a1),
          BigInt(a2),
          BigInt(a3),
          BigInt(a4),
          BigInt(a5)
        );
        const blocked = kernel.exports.minix_proc_blocked(spec.slot) === 1;

        if (blocked) {
          st.blockedCount += 1;
          st.pending = { value: result, nr, msgAddr };
          st.inst.exports.asyncify_start_unwind(st.dataPtr);
          return 0n;
        }
        return result;
      },
    },
  };
}

/// Give a slot an instance to run: a fresh memory, the imports above, the module's entry, and
/// its arguments.
///
/// Called once per slot at boot and again by exec, which is the whole reason it is a function
/// of the *slot*: exec replaces what a slot runs — its address space, its entry, its
/// arguments — without changing which slot it is, and without the kernel having to know that
/// the host did it.
function instantiate(st, module, argv, entry) {
  st.memory = new WebAssembly.Memory({ initial: 256, maximum: 4096 });
  st.inst = new WebAssembly.Instance(module, makeImports(st));
  // The module names its own scratch region; it is not derived from a linker
  // default. The 16-byte Asyncify struct goes at its start and the unwind stack
  // follows.
  st.scratch = st.inst.exports.asyncify_scratch_ptr();
  st.dataPtr = st.scratch;
  const bufStart = st.dataPtr + STRUCT_SIZE;
  writeU32(st.memory, st.dataPtr + 0, bufStart);
  writeU32(st.memory, st.dataPtr + 4, bufStart + BUF_SIZE);
  writeU32(st.memory, st.dataPtr + 8, bufStart);
  // The entry is the *module's*, not the slot's: `minix_init` and `minix_program_main` belong
  // to different modules, and which one a slot runs is what changes at exec.
  st.entry = entry ?? st.spec.entry;
  // Which module this slot is running, so a fork can instantiate the same one: the child's
  // memory is a copy of this module's, and a child running different code would be nonsense.
  st.module = module;
  // What the entry is called with. A server's entry takes nothing; a program's takes a C
  // argv, which is only knowable once the instance exists — the argv area is a static inside
  // it. A module instantiated by exec gets the arguments the caller exec'd with.
  st.entryArgs = [];
  if (argv !== undefined) {
    st.entryArgs = [argv.length, writeArgv(st, argv) + 4];
  }
  // Whatever the replaced image was waiting on went with it: an exec does not return to the
  // syscall it replaced.
  st.pending = null;
}

function makeServer(spec) {
  const st = {
    spec,
    memory: null,
    inst: null,
    module: null,
    started: false,
    exited: false,
    pending: null,
    blockedCount: 0,
    dataPtr: 0,
    scratch: 0,
    syscalls: 0,
    entry: spec.entry,
    entryArgs: [],
    // Where the instance running here now came from: `undefined` for one the harness created
    // at boot, and a record of the exec for one the kernel asked for.
    exec: undefined,
    // The instance (and memory) this slot was running before an exec, kept so the checks can
    // read what the replaced image left in it.
    prev: undefined,
    // Set on a slot the kernel created by forking: the parent's slot. `undefined` for every
    // slot that came from `specs` or from an exec.
    forkOf: undefined,
    trace: [],
    // The *last* few syscalls, as well as the first `TRACE_LIMIT`. The checks that
    // ask "is it in its main loop?" are asking about the tail, but a head-only
    // trace answers with whichever server made the most calls before it looped:
    // VM queries the kernel about every process slot first (`vm_init_boot`), which
    // is 256 calls, so a 64-entry head never reaches its `RECEIVE`. The client
    // checks need the head, so both are kept.
    tail: [],
  };

  instantiate(st, spec.module ?? serverModule, spec.argv);

  // The boot image reaches the RAM disk instance's memory before its entry point runs (see
  // below), so nothing else may grow that memory between here and there.
  return st;
}

/// The host's process table lookup: one record per slot the kernel knows a process by.
function procAt(slot) {
  return procs.find((p) => p.spec.slot === slot);
}

/// Give `childSlot` a copy of `parentSlot`'s process, as `fork` does.
///
/// `tools/fork-spike/` is the authority on this, and it found four things that are silent when
/// wrong. All four are here, and they are the whole of the host's part:
///
/// 1. The snapshot goes in **after** `instantiate()`, because instantiation re-applies the
///    module's data segments. A child that never receives the snapshot starts from the entry
///    point again — and re-issues the fork, forking recursively.
/// 2. The child's memory is the parent's *size*, not the module's minimum: a parent that grew
///    past the minimum would otherwise be truncated at the fork point.
/// 3. The host-side record is duplicated, not just the memory — memory is not the whole process.
///    In particular the child inherits `pending`: it is suspended inside the same syscall the
///    parent is, and that is what makes its next dispatch a *rewind* rather than a start.
/// 4. The divergent return values are not written here at all. The resume path already returns
///    `minix_proc_retval(slot)`, which the kernel set to 0 for the child, and PM's reply decides
///    the parent's. Nothing about a fork is asymmetric except which of those two arrives.
function forkSlot(parent, childSlot) {
  // A spec-like record rather than a boot spec: the kernel made this slot, so there is no boot
  // order, no argv of its own (the snapshot carries the parent's argparse area) and no endpoint
  // the host was ever told. `startAfterBoot` keeps it out of the boot spawn loop.
  const child = makeServer({
    slot: childSlot,
    entry: parent.entry,
    label: `${parent.spec.label}+F`,
    module: parent.module,
    startAfterBoot: true,
  });

  // (1) and (2): after instantiation, and the parent's size.
  const parentPages = parent.memory.buffer.byteLength / WASM_PAGE;
  const childPages = child.memory.buffer.byteLength / WASM_PAGE;
  if (parentPages > childPages) child.memory.grow(parentPages - childPages);
  new Uint8Array(child.memory.buffer, 0, parent.memory.buffer.byteLength).set(
    new Uint8Array(parent.memory.buffer)
  );

  // (3): the record. The copies are shallow for the syscall pairs, which nothing mutates in
  // place, and deep for the histories, which the child goes on appending to.
  child.pending = parent.pending === null ? null : { ...parent.pending };
  child.entryArgs = [...parent.entryArgs];
  child.tail = parent.tail.map((t) => ({ ...t }));
  child.trace = parent.trace.map((t) => ({ ...t }));
  child.syscalls = parent.syscalls;
  child.blockedCount = parent.blockedCount;
  child.forkOf = parent.spec.slot;

  // `exec` and `prev` stay unset: they are what the checks read as "this slot was replaced by an
  // exec", and the child has neither exec'd nor been replaced.
  return child;
}

/// The host's half of fork: clone the process in `parentSlot` into `childSlot`.
///
/// Called from the kernel's fork arm, which is the only layer that knows a fork happened — PM
/// drove it and VM chose the slot, and neither can copy a memory it does not own. The host is
/// asked because it owns both (§5.1), and a fork here is nothing more than bytes: there is no
/// page table to copy, because on this port an address space *is* an instance's memory.
///
/// Returns 0, or a negative errno. `ENOMEM` covers both "the clone failed" and "there is
/// already a process in that slot": the kernel checks the slot too, so reaching the second one
/// means the host and the kernel disagree about the process table, which is worth a note.
function hostForkProcess(parentSlot, childSlot) {
  const parent = procAt(parentSlot);
  if (parent === undefined || parent.inst === null) {
    note('the kernel asked to fork a slot the host does not have', `parent=${parentSlot}`);
    return EINVAL;
  }
  const occupant = procAt(childSlot);
  if (occupant !== undefined && !occupant.exited) {
    note('the kernel forked into a slot the host is already running', `child=${childSlot}`);
    return EINVAL;
  }
  // A slot whose process has exited is a free slot, and the kernel is entitled to hand it to
  // the next fork: its `Proc` went SLOT_FREE when the process died, and PM's table freed the
  // mproc with it. The host has to reach the same conclusion, and the record is the thing that
  // moves — dropping the dead one keeps `procs` one entry per live slot, which is what both
  // `procAt` and the dispatch loop's slot lookup require. The *fork* is recorded below and
  // carries the child's own record, so a check that asks `the child of this fork` still names
  // the process it means after the slot has been reused.
  if (occupant !== undefined) procs.splice(procs.indexOf(occupant), 1);

  let child;
  try {
    child = forkSlot(parent, childSlot);
  } catch (e) {
    note('cloning a process for fork failed', `${parentSlot} -> ${childSlot}: ${e}`);
    return ENOMEM;
  }
  // The spike's invariant 2, asserted rather than trusted: over the parent's whole length the
  // child's memory is the parent's, byte for byte. A copy that was short by a page would
  // truncate whatever the parent had grown past the module's minimum, and nothing downstream
  // would report the truncation — the child would simply resume into a hole. This is the one
  // step of the clone with no second opinion, so it is compared rather than assumed.
  const parentBytes = parent.memory.buffer.byteLength;
  const childBytes = child.memory.buffer.byteLength;
  const exact =
    childBytes >= parentBytes &&
    Buffer.compare(
      Buffer.from(new Uint8Array(child.memory.buffer, 0, parentBytes)),
      Buffer.from(new Uint8Array(parent.memory.buffer, 0, parentBytes))
    ) === 0;
  procs.push(child);
  forks.push({
    parent: parentSlot,
    child: childSlot,
    // The child's own record, not a lookup by slot: a slot is a place, and the process that
    // lived there can be gone by the time a check asks (a later fork reuses it).
    st: child,
    atSyscall: parent.syscalls,
    exact,
    parentBytes,
    childBytes,
  });
  return 0;
}

const procs = specs.map((s) => makeServer(s));

// ------------------------------------------------ the boot filesystem image
//
// The kernel maps the MinixFS image into the RAM disk server's address space
// before that server starts. There is no kernel mapping here, so the host writes
// it: the instance is grown to cover the window and the image goes in at
// `RAMDISK_IMAGE_VA`. It must land before the server's entry point runs, since
// that is when the device is sized from the image's own superblock.
const ramdiskImage = fs.readFileSync(ramdiskImagePath);
{
  const st = procs.find((p) => p.spec.label === 'ramdisk');
  const needPages = Math.ceil((RAMDISK_IMAGE_VA + ramdiskImage.length) / WASM_PAGE);
  const havePages = st.memory.buffer.byteLength / WASM_PAGE;
  if (needPages > havePages) st.memory.grow(needPages - havePages);
  new Uint8Array(st.memory.buffer, RAMDISK_IMAGE_VA, ramdiskImage.length).set(
    ramdiskImage
  );
  // Read the size back the way the server does: the superblock's `s_nzones` at
  // image offset 1028, times 4096. If the host and the server disagreed about
  // where the image is, this is where it shows.
  const view = new DataView(st.memory.buffer);
  const magic = view.getUint16(RAMDISK_IMAGE_VA + 1024 + 28, true);
  const nzones = view.getUint32(RAMDISK_IMAGE_VA + 1024 + 4, true);
  check(
    'the boot image is in the ram disk instance, and describes its own size',
    magic === 0x4d5a && nzones * 4096 === ramdiskImage.length,
    `magic=0x${magic.toString(16)} s_nzones=${nzones} -> ${nzones * 4096}, image=${ramdiskImage.length}`
  );
  note(
    'what the ram disk instance was given',
    `image=${ramdiskImage.length} bytes at 0x${RAMDISK_IMAGE_VA.toString(16)}, ` +
      `memory grown to ${st.memory.buffer.byteLength / WASM_PAGE} wasm pages`
  );
}

// A spec marked `startAfterBoot` is not a boot process: the host creates it once the boot
// chain has converged, which is what an exec does on the shipping arches (the kernel asks, and
// the host instantiates). Here the harness asks, so M7a step 1 is the module being real rather
// than the request being real — but the timing has to match either way, and not only for
// faithfulness: a program spawned into the boot run queue is *scheduled* during boot, and this
// one ran while INIT was blocked on its `getpid` reply, splitting INIT's console line in half.
// The kernel was right and the harness was early.
function spawnInstance(s) {
  return s.spawnAsUser
    ? kernel.exports.minix_proc_spawn_user(s.slot, s.endpoint)
    : kernel.exports.minix_proc_spawn(s.slot, s.endpoint);
}

const bootSpecs = specs.filter((s) => !s.startAfterBoot);
const startSpecs = specs.filter((s) => s.startAfterBoot);
const startFailures = [];

const spawnFailures = bootSpecs.filter((s) => spawnInstance(s) !== 0);
check(
  'the kernel accepts every boot instance into its table',
  spawnFailures.length === 0,
  spawnFailures.map((s) => s.label).join(', ')
);

note(
  'what each instance claims for itself',
  procs
    .map(
      (p) =>
        `${p.spec.label}: scratch=0x${p.scratch.toString(16)}, memory=${p.memory.buffer.byteLength}`
    )
    .join('; ')
);

// Start PM's chain. This is the kernel-state step `boot_init` performs on the
// shipping arches: RS's notification is left pending on PM's privilege structure
// and PM finds it on its next RECEIVE. It is deliberately not a message — nothing
// is sent between instances — which is why it is a single bit to set rather than
// a copy.
const notified = kernel.exports.minix_boot_notify() === 0;
check(
  'the kernel sets RS boot notification on PM',
  notified,
  'minix_boot_notify() could not reach the privilege structure'
);

// ---------------------------------------------------------- dispatch loop

function run(st) {
  if (st.pending !== null) {
    st.inst.exports.asyncify_start_rewind(st.dataPtr);
  }
  st.started = true;
  st.inst.exports[st.entry](...st.entryArgs);
  const state = st.inst.exports.asyncify_get_state();
  if (state === STATE_UNWINDING) return;
  st.exited = true;
  kernel.exports.minix_proc_exit(st.spec.slot);
}

let steps = 0;
let converged = true;
for (;;) {
  const slot = kernel.exports.minix_step();
  if (slot === -1) {
    // Nothing is runnable, which for this harness is what "the boot chain converged" means. It
    // is also the moment the host owes the `startAfterBoot` instances: a program must not be in
    // the run queue during boot, or the kernel schedules it in the middle of another process's
    // console output — which is what this harness did first, splitting INIT's `pid=` line while
    // INIT was blocked on the reply. Once, then dispatch again; the second quiescence is the end.
    if (startSpecs.length > 0) {
      for (const s of startSpecs.splice(0)) {
        if (spawnInstance(s) !== 0) startFailures.push(s);
      }
      continue;
    }
    break;
  }
  // A safety net, not a bound: the exchange adds round trips, and a resume
  // consumes a step like a first run does. A runaway is caught by the syscall
  // budget instead, which names the instance -- and now throws rather than
  // returning an error, so a loop that never comes back here still ends.
  //
  // The number has to leave room for a *reader*. Every console byte the shell
  // consumes costs several dispatches (shell -> VFS -> tty -> VFS -> shell), so a
  // 31-byte script is a few hundred steps on its own; 256 fitted a boot that only
  // ever wrote to the console and cut the shell off mid-line (M3f).
  if (++steps > 4000) {
    converged = false;
    break;
  }
  const st = procs.find((p) => p.spec.slot === slot);
  if (st === undefined) {
    converged = false;
    note('the kernel picked a slot the host never spawned', `slot ${slot}`);
    break;
  }
  // Bounded step trace. A deadlock shows up here as the same slot being picked
  // over and over with its syscall count frozen, which is the only way to tell a
  // stuck instance from one that is merely slow -- and nothing else prints until
  // the loop converges. `WASM_TRACE=1` opts in; it is noise on a normal run.
  if (process.env.WASM_TRACE === '1' && steps <= 300) {
    const last = st.tail.length > 0 ? st.tail[st.tail.length - 1] : null;
    console.log(
      `  step ${steps}: slot ${slot} (${st.spec.label}) syscalls=${st.syscalls} ` +
        `last=${last === null ? '-' : `nr=${last.nr} a0=0x${last.a0.toString(16)}`}`
    );
  }
  try {
    run(st);
  } catch (e) {
    if (e instanceof BudgetExhausted) {
      // The budget is the host's only lever against a runaway guest (finding 12), and this
      // is the form of it that works when the runaway never returns to this loop: stop the
      // run and say so, rather than ending it from outside with no diagnosis.
      converged = false;
      note('the syscall budget ran out inside a single dispatch', e.message);
      break;
    }
    // A trap whose last syscall was EXIT is a process exiting, not failing.
    // `minix-rt::exit` issues `SYS_EXIT` and then traps, because this target gives the
    // host no chance to run while an instance is executing, so a spin would hang it
    // synchronously rather than fail (finding 12). The kernel has already done the exit
    // bookkeeping -- SIGNALED | SIG_PENDING | SLOT_FREE, the queued exit status, and a
    // notification to PM as signal manager -- so recording it is all that is left.
    // It must *not* call `minix_proc_exit` here: that stores SLOT_FREE on its own,
    // which would clear SIGNALED, and SIGNALED is exactly what PM's GETKSIG loop
    // looks for to find the exit.
    const last = st.tail.length > 0 ? st.tail[st.tail.length - 1] : null;
    if (last !== null && last.nr === EXIT) {
      st.exited = true;
      continue;
    }
    // A trap is how a wasm process dies loudly: `minix-rt`'s panic handler exits, and
    // `exit` now traps instead of spinning, so a panic arrives here as
    // `RuntimeError` rather than as a hang nothing can report. The instance's tail is
    // the whole diagnosis -- which syscall it was in, and what it had done -- so print
    // it before letting the error through.
    console.log(
      `\nTRAP in ${st.spec.label} (slot ${st.spec.slot}) at step ${steps}: ${e}`
    );
    console.log(`  syscalls=${st.syscalls}`);
    console.log(`  first ${st.trace.length} syscalls: ${st.trace.map((t) => t.nr).join(',')}`);
    console.log(
      `  last: ${st.tail.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join(', ')}`
    );
    // The console timeline is only printed at the end of a successful run, so a
    // panic's message — which reaches it as the dying process's `write(2, ...)` —
    // would be thrown away exactly when it is most wanted. Print what has
    // accumulated, partial line included.
    if (currentLine !== '') console.log(`${consoleTag}: ${currentLine}`);
    console.log('--- console timeline up to the trap ---');
    for (const line of timeline) console.log(`  ${line}`);
    throw e;
  }
}

check('the dispatch loop converged', converged, `${steps} steps`);

// ------------------------------------------------------------- assertions

const servers = procs.filter((p) =>
  [
    'ds',
    'rs',
    'pm',
    'ramdisk',
    'vm',
    'mfs',
    'virtio_blk',
    'devman',
    'vfs',
    'tty',
  ].includes(p.spec.label)
);
const client = procs.find((p) => p.spec.label === 'client');
const unregistered = procs.find((p) => p.spec.label === 'unregistered');
const ds = procs.find((p) => p.spec.label === 'ds');
const rs = procs.find((p) => p.spec.label === 'rs');
const ramdiskInst = procs.find((p) => p.spec.label === 'ramdisk');

// The last syscall a server makes before it stops is the `RECEIVE` it blocks in,
// which is what "it is in its main loop" means for an instance that no longer has
// a first-syscall-is-a-receive shape: RS does real work at init now — it registers
// its grant table and hands DS the process table — so its trace starts with that
// work and ends in the loop. A resume re-enters the syscall import, so a trace
// grows duplicates at every unwind and only its tail is meaningful here — which is
// why this reads `tail` and not `trace`.
const lastSyscall = (p) => p.tail[p.tail.length - 1];
const reachedLoop = servers.filter(
  (p) => p.tail.length > 0 && lastSyscall(p).nr === RECEIVE
);
check(
  'every server reached its main loop',
  reachedLoop.length === servers.length,
  servers
    .map((p) => `${p.spec.label}: ${p.tail.map((t) => t.nr).join(',') || '(no syscalls)'}`)
    .join('; ')
);

check(
  'each reached it by blocking in the kernel on a receive',
  servers.every((p) => kernel.exports.minix_proc_blocked(p.spec.slot) === 1),
  servers
    .map((p) => `${p.spec.label}: blocked=${kernel.exports.minix_proc_blocked(p.spec.slot)}`)
    .join('; ')
);

check(
  'each is receiving from any sender, as its main loop asks',
  servers.every((p) => {
    const receives = p.tail.filter((t) => t.nr === RECEIVE);
    return receives.length > 0 && receives[receives.length - 1].a0 === ANY;
  }),
  servers
    .map((p) => {
      const receives = p.tail.filter((t) => t.nr === RECEIVE);
      return `${p.spec.label}: a0=${receives[receives.length - 1]?.a0}`;
    })
    .join('; ')
);

check(
  'the RAM disk sized its device from the image, not from a constant',
  ramdiskInst.inst.exports.minix_ramdisk_device_size() === ramdiskImage.length,
  `size=${ramdiskInst.inst.exports.minix_ramdisk_device_size()} ` +
    `(image is ${ramdiskImage.length}; host put it at 0x${RAMDISK_IMAGE_VA.toString(16)}, ` +
    `and the driver reports base=0x${ramdiskInst.inst.exports.minix_ramdisk_device_base().toString(16)} ` +
    'because `ramdisk_set_image` stores the address in `dev.data` and zeroes `dev.base`)'
);

// The probe is gone, and this is what says so. `mount_root` prefers the `virtio_blk`
// driver and asks it whether it has a device; with that instance absent the probe
// blocked on a peer that would never answer, MFS sat inside it and the RAM disk was
// never asked for a block (`PORTING_PLAN.md` finding 25). A `SEND` in the RAM disk's
// trace can only happen after it received a BDEV request and answered, so this pins
// the whole chain in one fact: VFS mounted, MFS read, the RAM disk served.
check(
  'the RAM disk served a BDEV request, so the mount chain reached it',
  ramdiskInst.tail.some((t) => t.nr === 46),
  `ramdisk tail: ${ramdiskInst.tail.map((t) => t.nr).join(',')} (46 is SEND)`
);
// VFS is in the `servers` list above now, so it is asserted to have reached its main
// loop with the rest. Its tail is six `SENDREC`s — readsuper to MFS, mount_devman to
// devman, and the block reads between them — then `SYS_BOOT_COMPLETE` (call 60) and the
// `RECEIVE` it waits in, which is what makes the mount a proven fact rather than an
// observed one.

// ------------------------------------------ M3e: the console's tty server

// Whether the tty registered the console is *devman's* state, not the tty's: what a
// driver registers is a change to devman's own device tree, and nothing about it
// crosses the host boundary. So it is read from devman's instance, and the count has
// to be non-zero there rather than inferred from the tty reaching its loop —
// `devman_add_device` retries while the tree is not up and gives up quietly on any
// other error, so a tty whose registration was refused and one whose registration
// landed look identical from the tty's side. The console is the only device anything
// registers here, and the tty is what registers it.
const tty = procs.find((p) => p.spec.label === 'tty');
const devmanInst = procs.find((p) => p.spec.label === 'devman');
const registered = devmanInst.inst.exports.minix_devman_device_count();
check(
  "the tty server's console registration reached devman's device tree",
  registered === 1,
  `devman holds ${registered} device(s) under its root, expected 1 (tty0)`
);

// And the exchange that put it there, seen from both ends: the tty sent to devman's
// endpoint, and devman answered the tty (46 is SEND). Either alone is one half of a
// round trip that the count above would have caught as a whole.
const ttyAskedDevman = tty.trace.some(
  (t) => t.nr === SENDREC && t.a0 === devmanInst.spec.endpoint
);
const devmanAnswered = devmanInst.trace.some((t) => t.nr === 46 && t.a0 === tty.spec.endpoint);
check(
  'the tty server asked devman to register the console, and devman answered',
  ttyAskedDevman && devmanAnswered,
  `tty sent=${ttyAskedDevman} devman answered=${devmanAnswered}`
);

// The client's own sequence is the claim: it asks the kernel who it is (that is
// `rs_up`'s `GET_WHOAMI`), announces itself to RS, and only then sends to DS.
// Order rather than equality: a resume re-enters the syscall import, so the trace
// repeats an entry per block and only the sequence of what came first is stable.
const clientSends = client.trace.filter((t) => t.nr === SENDREC);
check(
  'the client announces itself to RS before it asks DS for anything',
  client.trace.length > 0 &&
    client.trace[0].nr === KERNEL_CALL &&
    client.trace[0].a0 === SYS_GETINFO &&
    clientSends.length >= 3 &&
    clientSends[0].a0 === rs.spec.slot &&
    clientSends.some((t) => t.a0 === ds.spec.slot),
  client.trace.map((t) => `nr=${t.nr} a0=${t.a0}`).join('; ') || '(no syscalls)'
);

// PM is the one that has something to do. Before it receives anything it asks the kernel for the
// image table (C's `sef_cb_init_fresh` calls `sys_getimage` first, and that loop is where PM's
// process table comes from — finding 39), and only then does `boot_init`'s pending notification
// bring it to its first RECEIVE. `boot_init` leaves RS's notification pending on PM's privilege
// structure, and PM finds it on that RECEIVE — then asks the kernel for pending signals, finds
// none, and goes back to waiting. That round trip is the chain starting to move, and it is visible
// only because the notification reached PM's own memory.
const pm = procs.find((p) => p.spec.label === 'pm');
// The claims, as properties rather than as a sequence of steps. The sequence assertion it used to
// be held exactly while no *user* process existed; INIT now sends PM a PM_GETPID and then an exit
// to report, so PM's trace legitimately has more in it -- SENDNB to endpoint 10, and a second
// GETKSIG round with the `SYS_ENDKSIG` that closes it. Pinning the whole trace was asserting where
// the boot had got to rather than what the protocol is, which is finding 27's lesson.
const pmSteps = pm.trace.map((t) => [t.nr, t.a0]);
// The image read is PM's first syscall of all: a kernel call, `SYS_GETINFO`, whose request
// (`GET_IMAGE`) lives inside the message. The trace records the call number, not the request.
const pmImageRead = pmSteps.findIndex(([nr, a0]) => nr === KERNEL_CALL && a0 === SYS_GETINFO);
const pmFirstGetksig = pmSteps.findIndex(([nr, a0]) => nr === KERNEL_CALL && a0 === GETKSIG);
const pmFirstReceive = pmSteps.findIndex(([nr, a0]) => nr === RECEIVE && a0 === ANY);
const pmRsAnswer = pmSteps.findIndex(([nr, a0]) => nr === SENDREC && a0 === rs.spec.endpoint);
// "Returned to receiving" is a claim about where PM *ends up*, and the head trace is not the
// record of that: `trace` stops at `TRACE_LIMIT`, and now that asynchronous sends are delivered
// (findings 37 and 38) PM serves more messages before it gets to the fork, so its 64th syscall
// is some mid-run SENDNB. The tail is the field that answers "what was it doing at the end" —
// the same question the server main-loop checks above ask of it.
const pmLastStep =
  pm.tail.length > 0 ? [pm.tail[pm.tail.length - 1].nr, pm.tail[pm.tail.length - 1].a0] : [0, 0];
check(
  'PM registers the kernel image, then consumes the boot notification, answers RS, and returns to receiving',
  pmImageRead === 0 &&
    pmFirstReceive > pmImageRead &&
    pmFirstGetksig > pmFirstReceive &&
    pmRsAnswer > pmFirstGetksig &&
    pmLastStep[0] === RECEIVE &&
    pmLastStep[1] === ANY,
  `head: ${pm.trace.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join('; ')}` +
    ` | tail: ${pm.tail.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join('; ')}`
);
// PM's copies, taken by *who they involve* rather than by position: RS's init now
// asks the kernel for two copies of its own (the `SYS_SETGRANT` message and its
// reply) before PM runs, so a prefix of the log is no longer PM's. What PM does before anything
// else crosses the seam is ask the kernel for its image table, and the table itself — kilobytes
// into a buffer of its own, not a 64-byte delivery slot — is the first thing that arrives. After
// it come the notification, PM's `SYS_GETKSIG` message being read *out of PM's memory*, and the
// reply going back. Before the kernel-call fix the middle of those did not exist — the kernel
// read its own memory in place of PM's message and dispatched on that.
const pmCopies = copyLog.filter((c) => c.srcProc === 0 || c.dstProc === 0);
const pmImageCopy = pmCopies.findIndex(
  (c) => c.srcProc < 0 && c.dstProc === 0 && c.bytes > 64
);
const pmFirstDelivery = pmCopies.findIndex((c) => c.srcProc < 0 && c.dstProc === 0);
check(
  'the image table, the notification, the kernel-call message and its reply all crossed the seam',
  pmCopies.length >= 4 &&
    pmCopies.every((c) => c.result === 0) &&
    pmImageCopy > 0 &&
    pmImageCopy === pmFirstDelivery &&
    pmCopies[pmImageCopy - 1].srcProc === 0 &&
    pmCopies[pmImageCopy - 1].dstProc < 0,
  pmCopies.map((c) => `${c.srcProc}->${c.dstProc}=${c.result}`).join(', ')
);
// And the init handshake PM is now part of, which is what makes it answer RS at all.
// Counted by *direction* rather than as a positional slice of the log: the handshake
// moves bytes out of RS's instance into the kernel's staging buffer and on into PM's,
// then PM's answer back the other way, and each of those four directions has to appear
// at least once with a successful copy. The slice-of-three this used to be needed the
// log to hold exactly three entries involving PM, which stopped being true the moment
// INIT added its own -- finding 27's lesson, again.
const dirCount = (src, dst) =>
  copyLog.filter((c) => c.srcProc === src && c.dstProc === dst && c.result === 0).length;
const rsInitDirs = [
  dirCount(rs.spec.slot, -1),
  dirCount(-1, pm.spec.slot),
  dirCount(pm.spec.slot, -1),
  dirCount(-1, rs.spec.slot),
];
check(
  "RS's init request, PM's answer and RS's reply all crossed the seam",
  rsInitDirs.every((n) => n >= 1),
  `copies by direction (rs->kernel, kernel->pm, pm->kernel, kernel->rs) = [${rsInitDirs.join(',')}]`
);

// ------------------------------------------------------------ the DS exchange

// The handshake that makes any of this possible, and the reason it is a grant
// rather than message bytes: the table is ~4.4 KiB of *RS's* memory, so the copy
// runs through the kernel's scope-bound seam (`SYS_SAFECOPYFROM`), and the grant
// entry `verify_grant` resolves is itself read out of RS's instance.
const grantEntry = copyLog.find((c) => c.srcProc === 2 && c.dstProc === -1 && c.bytes === 48);
check(
  "the kernel read RS's grant entry out of RS's own instance",
  grantEntry !== undefined && grantEntry.result === 0,
  copyLog
    .filter((c) => c.srcProc === 2 || c.dstProc === 2)
    .map((c) => `${c.srcProc}:0x${c.srcAddr.toString(16)}->${c.dstProc}:${c.bytes}=${c.result}`)
    .join(', ')
);
// 32 slots of `RprocPub` (140 bytes each on this 32-bit target).
const RPUB_TABLE_BYTES = 4480;
const tableCopy = copyLog.find(
  (c) => c.srcProc === 2 && c.dstProc === 6 && c.bytes === RPUB_TABLE_BYTES
);
check(
  "DS copied RS's public process table out of RS's instance",
  tableCopy !== undefined && tableCopy.result === 0,
  copyLog
    .filter((c) => c.dstProc === 6)
    .map((c) => `${c.srcProc}:0x${c.srcAddr.toString(16)}->6 ${c.bytes}b=${c.result}`)
    .join(', ')
);

// What the client saw, read from the client's own instance: it has no console
// import, so a report in memory is the only channel it has.
const readReport = (st, n) => {
  const view = new DataView(st.memory.buffer);
  const ptr = st.inst.exports.ds_report_ptr();
  return Array.from({ length: n }, (_, i) => Number(view.getBigInt64(ptr + i * 8, true)));
};
const dsReport = readReport(client, 5);

const dsReplies = ds.trace.filter((t) => t.nr === SENDNB).length;
check(
  'DS answered the handshake and the client',
  dsReplies >= 3,
  `${dsReplies} reply syscall(s) in DS's trace`
);

// The other direction of the init protocol, which is what the init-complete reply
// closes: DS answers the request RS sent it, and RS's `do_init_ready` consumes the
// answer. RS's only SENDNB to DS's endpoint is that reply — its other replies go to
// the client — and it is sent after DS's table copy, so the two checks below are
// the request and its answer rather than two views of the same message.
const rsInitReply = rs.trace.filter((t) => t.nr === SENDNB && t.a0 === ds.spec.endpoint);
check(
  'RS answered DS\'s init-complete reply',
  rsInitReply.length === 1,
  rs.trace.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join('; ')
);

// And the state the reply was supposed to change: a flag inside RS, which no copy
// or trace can show, so the host asks RS. Read from RS's own instance — each
// instance carries all three servers' code, so an untouched table would answer 0.
const dsActive = rs.inst.exports.minix_rs_is_active(ds.spec.endpoint);
check(
  'RS moved DS out of RS_INITIALIZING, so the reply was consumed',
  dsActive === 1,
  `minix_rs_is_active(${ds.spec.endpoint}) = ${dsActive}`
);

// The same handshake for the other service RS asks. PM answers in its main loop,
// which is where the request arrives, so this also pins that a service can be
// *asked* rather than assumed ready.
const rsPmReply = rs.trace.filter((t) => t.nr === SENDNB && t.a0 === pm.spec.endpoint);
check(
  "RS answered PM's init-complete reply",
  rsPmReply.length === 1,
  rs.trace.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join('; ')
);
const pmActive = rs.inst.exports.minix_rs_is_active(pm.spec.endpoint);
check(
  'RS moved PM out of RS_INITIALIZING, so its reply was consumed',
  pmActive === 1,
  `minix_rs_is_active(${pm.spec.endpoint}) = ${pmActive}`
);

// The two services the wasm work had to add to RS's own table before it could ask
// them at all. `lookup_slot_by_endpoint` scans `boot_svcs` and nothing else, so
// without those entries RS panicked instead of asking (finding 27); with them, the
// handshake runs and — the point — RS's own view of each ends up active. Asking
// them is also what exercises `minix_util::rs::answer_rs_init` in the RAM disk and
// the virtio block driver, which is otherwise unreachable code.
for (const label of ['ramdisk', 'virtio_blk', 'devman', 'mfs', 'vfs']) {
  const inst = procs.find((p) => p.spec.label === label);
  const asked = rs.trace.some((t) => t.nr === SEND && t.a0 === inst.spec.endpoint);
  const active = rs.inst.exports.minix_rs_is_active(inst.spec.endpoint) === 1;
  check(
    `RS asked ${label} to initialise and it answered`,
    asked && active,
    `sent=${asked} active=${active} (endpoint ${inst.spec.endpoint})`
  );
}

// The round trip this harness exists for, now that DS can name the client: RS
// registered the label (`rs_up`), published it to DS, and DS accepted both the
// publish and the read-back.
check(
  'the client announced itself to RS',
  dsReport[3] === 0,
  `rs_up=${dsReport[3]} (expected 0)`
);
// A known service may still not declare *itself* ready: RS is the one that asks,
// so an unsolicited `RS_INIT` from a slot it never put into RS_INITIALIZING is
// refused. EINVAL, as C answers it — not ESRCH, which would mean RS had never
// heard of the client at all.
check(
  'RS refuses an init-complete reply it did not ask for',
  dsReport[4] === -22,
  `unsolicited RS_INIT=${dsReport[4]} (expected -22 EINVAL)`
);
check(
  'DS accepted the client\'s publish and handed the value back',
  dsReport[0] === 0 && dsReport[2] === 0x2a,
  `publish=${dsReport[0]} retrieve=${dsReport[1]} value=0x${dsReport[2].toString(16)}`
);

// And the refusal survives: the second client runs the same protocol against the
// same key without announcing itself, so DS has no label for it.
const unregReport = readReport(unregistered, 4);
check(
  'DS refuses a publisher it has no label for, as the reference does',
  unregReport[0] === -1,
  `publish=${unregReport[0]} (expected -1 EPERM)`
);

// The key is a `const` in the client's own instance, so DS can only have it
// because the kernel asked the host to copy it across — the one thing that makes
// this a two-instance exchange rather than a server answering itself. One copy
// per request, and the byte count is the key's own length.
const keyCopies = copyLog.filter(
  (c) =>
    c.srcProc === client.spec.slot &&
    c.dstProc === ds.spec.slot &&
    c.bytes === 8 &&
    c.result === 0
);
check(
  "DS read the key out of the client's instance through the seam",
  keyCopies.length === 2,
  copyLog.map((c) => `${c.srcProc}->${c.dstProc}:${c.bytes}=${c.result}`).join(', ')
);

check(
  'no instance ran the syscall budget out',
  exhaustedBy === null,
  `${exhaustedBy} spun past ${SYSCALL_BUDGET} syscalls`
);

check(
  "the kernel's run queues are consistent afterwards",
  kernel.exports.minix_runqueues_ok() === 1,
  'runqueues_ok() reported an inconsistency'
);

// ----------------------------------------- M3d: the first user process (INIT)

const init = procs.find((p) => p.spec.label === 'init');

// The claim is not "an instance ran" but "an ordinary user process ran", and the
// difference between that and a server is kernel state, so ask the kernel instead
// of inferring it from the slot number. Finding 9 is the shape of getting this
// wrong without noticing: the wasm kernel once ran no boot sequence at all, so no
// process had a privilege structure and every check still passed.
const initKind = kernel.exports.minix_proc_kind(init.spec.slot);
check(
  'the kernel made INIT an ordinary user process rather than a server',
  initKind === 1,
  `kind=${initKind} (1 = shared USER slot, 2 = SYS_PROC, 0 = no priv)`
);
// The control, so the answer above cannot be a constant.
const dsKind = kernel.exports.minix_proc_kind(ds.spec.slot);
check(
  'the same question about DS answers "server"',
  dsKind === 2,
  `kind=${dsKind}`
);

// Both of INIT's effects had to cross an instance boundary. The console lines exist
// only if the kernel read them out of INIT's own memory through the copy seam
// (findings 28 and 29), and the pid is not a kernel syscall: `minix-rt`'s `getpid`
// is a SENDREC to PM, so a number here means the shared USER slot let an ordinary
// user reach PM and PM answered. The value pins PM's own answer rather than a
// constant -- PM assigns `mp_pid = endpoint + 1` when it fills its boot table, so
// INIT at endpoint 10 is pid 11. (C gives init `INIT_PID` of 1; that is not this
// port's scheme, and changing it is not this milestone's business.)
const sawBanner = timeline.some((l) => l.includes('init: booting MINIX/Rust'));
// A line printed *before* `set_fd_vfs`, so it can only have arrived by the shortcut:
// afterwards INIT's writes go the long way and the checks below take over.
const sawShortcut = timeline.some((l) => l.includes('init: open(/dev/console) ->'));
check(
  "INIT's console output reached the harness through the kernel",
  sawBanner && sawShortcut,
  `banner=${sawBanner} shortcutLine=${sawShortcut}`
);
const pidLine = timeline.find((l) => l.includes('init: pid='));
check(
  'INIT reached PM over SENDREC and PM answered with its pid',
  pidLine !== undefined && pidLine.endsWith('init: pid=11'),
  `line=${JSON.stringify(pidLine)} (expected one ending "init: pid=11")`
);

// And the process ended by exiting rather than by trapping: the last syscall in the slot's
// whole history is EXIT, which is the only thing that lets the harness tell the two traps
// apart. Whose exit it is matters and is not INIT's: `minix_init`'s last step is the exec,
// which replaces it, so this is the exit the *shell* takes in the same slot one image later.
// A check saying so is below (M7a step 2), including the negative control that says the
// replacement happened rather than the failed-exec path being taken.
const initTail = init.tail.map((t) => t.nr);
check(
  'the process in INIT\'s slot exited through the exit syscall',
  init.exited === true && initTail[initTail.length - 1] === EXIT,
  `exited=${init.exited} tail=[${initTail.join(',')}]`
);

/// The instance a slot was running **before** an exec, or the one it is running now.
///
/// The M3e reports are values INIT wrote before it exec'd, so reading them from `st.inst`
/// after the run would ask the *replacement* module for exports it has never heard of.
function beforeExec(st) {
  return st.prev === undefined ? { inst: st.inst, memory: st.memory } : st.prev;
}

note(
  'what M3d establishes, and what M3 still owes',
  'INIT is the first process on this port that is not a server: the kernel links it ' +
    'to the shared USER privilege slot, its console output leaves through the ' +
    'kernel\'s copy seam, and it reaches PM with a real SENDREC. It used to exit through ' +
    'SYS_EXIT because the shell ran in place of an exec; now its last step is the exec ' +
    'itself, so the exit at the end of the slot belongs to the shell (M7a step 2), and the ' +
    'module it exec\'d came off the boot image (step 4). What is still owed is M7\'s other ' +
    'half: a shell can only run its builtins until fork exists.'
);

// ----------------------------------- M3e: INIT's stdio, through VFS and the tty

// Read from INIT's own report rather than off the console, because a line on the
// console would also be there if the kernel's shortcut had served it. Three values:
// the open's descriptor (or a negated errno), the dup2 status, and the byte count the
// VFS-routed write returned. A step the run never reached stays at its sentinel, so
// "not attempted" cannot be read as "succeeded with zero" -- which is exactly how the
// dup2 check below passed while the open was still failing.
const initReport = (() => {
  const before = beforeExec(init);
  const view = new DataView(before.memory.buffer);
  const ptr = before.inst.exports.init_report_ptr();
  return Array.from({ length: 3 }, (_, i) => Number(view.getBigInt64(ptr + i * 8, true)));
})();
check(
  'INIT opened /dev/console through VFS to the tty',
  initReport[0] >= 0,
  `open status=${initReport[0]} (>=0 is a descriptor, negative is an errno)`
);
// The dup2 is what puts a filp on fd 1; without it the VFS-routed write below would
// answer EBADF rather than reaching a driver, so this is a real precondition and not
// bookkeeping. `-4096` here means the step was never reached.
check(
  "INIT dup2'd the console onto 0..2",
  initReport[1] === 0,
  `dup2 status=${initReport[1]} (-4096 = never reached)`
);
// The milestone: this write did *not* take the kernel's console shortcut. It left as a
// VFS_WRITE, VFS vircopied the bytes out of INIT's instance into a CDEV_WRITE message,
// and the tty wrote them with its own write(1) -- so both the count and the line are
// evidence, and neither alone would be.
const VFS_LINE = 'init: stdio is VFS-routed';
const VFS_LINE_BYTES = VFS_LINE.length + 2; // + CRLF
const sawVfsLine = timeline.some((l) => l.includes(VFS_LINE));
check(
  'a write from INIT reached the console through VFS and the tty',
  initReport[2] === VFS_LINE_BYTES && sawVfsLine,
  `write status=${initReport[2]} (expected ${VFS_LINE_BYTES}), line on console=${sawVfsLine}`
);

// --------------------------------------------- M3f: the shell, in INIT's slot

// init's last step is `exec("/bin/sh")`, so the shell arrives as a *module* the host
// instantiates into the same slot, carrying the stdio above with it: exec keeps the process,
// so the fds, `p_fd_vfs` and the VFS filps are all still there and the shell opens nothing.
// The swap itself is checked in the M7a step 2 section below; these two lines are what the
// *replacement* image did with the console, and they are different claims: the prompt says a
// reader started and its first write went out through VFS and the tty; the builtin's output
// says a whole line came back in, was parsed, and its result was written out. The host
// supplies that line (`consoleInput`), so the input direction is exercised rather than
// assumed.
const sawPrompt = timeline.some((l) => l.includes('# '));
// Exactly, not `includes`: the tty echoes the input, so the *echo* of the command also
// contains this text on the prompt line. Only the builtin's own output is a line of its
// own that is nothing but this.
const BUILTIN_LINE = `kernel: ${BUILTIN_ARGS.join(' ')}`;
const sawBuiltin = timeline.some((l) => l.trimEnd() === BUILTIN_LINE);
check(
  'the shell printed a prompt through VFS and the tty',
  sawPrompt,
  `prompt on console=${sawPrompt}`
);
check(
  'the shell read a line, ran its echo builtin, and its output reached the console',
  sawBuiltin,
  `looked for ${JSON.stringify(BUILTIN_LINE)}: builtin output on console=${sawBuiltin}`
);

// ------------------------- M7a step 1: a program as its own wasm module

// M3f worked *around* exec: the shell ran in INIT's slot, in `wasm-servers`, because there
// was no way to run a program that is not a server. This is step 1 of the M7a plan and it is
// the smallest thing that changes that: a program is its own module, the host instantiates it
// for a slot and hands it argv, and it runs as an ordinary user process.
//
// The three facts are M3d's, asked again because this instance is not `wasm-servers`: its
// module, its memory layout and its argv area are all its own, so nothing established about
// the servers carries over to it.
const program = procs.find((p) => p.spec.label === 'program');

// The spawn is the harness's, not the kernel's: `startAfterBoot` instances are created at the
// first quiescence, which is the host's hand-rolled half of §7.2's step 2. A failure here would
// make every check below meaningless, so it is asserted first.
check(
  'the kernel accepts the program into its table as an ordinary user process',
  startFailures.length === 0,
  startFailures.map((s) => s.label).join(', ')
);

// A slot outside `BOOT_IMAGE` has no privilege structure, and `minix_proc_spawn` would leave
// it that way — a process that cannot send to PM, VFS or DS at all, which is not the kind of
// process exec produces. The control is the same question about DS, asked above: `2` there
// and `1` here is what says the kernel attached the *shared USER* slot rather than a private
// one.
const programKind = kernel.exports.minix_proc_kind(program.spec.slot);
check(
  'the kernel made the program an ordinary user process rather than a server',
  programKind === 1,
  `kind=${programKind} (1 = shared USER slot, 2 = SYS_PROC, 0 = no priv)`
);

const programTail = program.tail.map((t) => t.nr);
check(
  'the program ran and exited through the exit syscall',
  program.exited === true && programTail[programTail.length - 1] === EXIT,
  `exited=${program.exited} tail=[${programTail.join(',')}]`
);

// `echo` prints its arguments, so this one line is three claims. That argv arrived in the
// instance: the module cannot print what it was never given, and it only reaches `echo` at all
// if argv[0] matched. That a program's output reaches the console through the kernel's
// shortcut: this process has opened no console, so `p_fd_vfs` is 0 and the kernel reads the
// bytes out of *this* instance through the copy seam (§5.1) — finding 29 is what a write like
// it that transfers `count = 0` looks like instead. And that the blob parsed: the host's
// array is the expectation, so a disagreement about the layout or the offsets shows up here
// rather than as a program that printed something plausible.
const PROGRAM_LINE = `kernel: ${PROGRAM_ARGV.slice(1).join(' ')}`;
const sawProgramLine = timeline.some((l) => l.trimEnd() === PROGRAM_LINE);
check(
  'the program read the argv the host wrote and its output reached the console',
  sawProgramLine,
  `looked for ${JSON.stringify(PROGRAM_LINE)}`
);

note(
  'what M7a step 1 establishes, and what it does not',
  'A program is now a wasm module of its own: `wasm-program` is a cdylib exporting one entry, ' +
    'the host Asyncifies it, instantiates it for a slot, writes argv into the argv area the ' +
    'module declares, and the entry parses it with the same `userland::parse_args` every ' +
    '`userland/src/bin/*.rs` uses on the shipping arches. What is *not* here is a caller: ' +
    'nothing asked the kernel to create this process, the harness did, and the slot was ' +
    'chosen by hand — which is exactly what step 2, below, changes. The harness also ' +
    'instantiates this one straight from the file it built, which is the harness\'s own ' +
    'business: an exec takes its bytes from the image instead (step 4).'
);

// ------------------------------- M7a step 2: the kernel asks, the host instantiates

// INIT's exec, end to end. The claim is not that the host *can* instantiate a module — step 1
// shows that — but that a process asked to become another program and the request travelled
// the real path: `execve` (INIT) → PM_EXEC (PM) → VFS_PM_EXEC (VFS resolves the path against
// the boot image and reads the executable) → SYS_EXEC_LOAD (the kernel's exec arm) → this
// import.
//
// Every hop of that has a check elsewhere; what is checked here is the last one, and the
// reason it is worth checking is that the kernel's arm is *only* reachable that way. The path
// and the byte count are what the *kernel* handed over — read out of its memory, not the
// harness's — and the argument list was parsed out of the exec frame by the kernel
// (`elf::parse_exec_frame`) after two cross-instance copies fetched it from the caller. So a
// path here is a request the kernel made, not one the harness made.
const exec = init.exec;
// The size is the image builder's claim: `mkminixfs wasm32` embedded exactly the module file the
// harness staged, so a byte count that matches it says the bytes the host compiled came off the
// disk rather than out of anything the harness kept on the side. Since §7.2's step 4 there is no
// host-side registry left to fall back on — the only module store is the image.
const MODULE_BYTES = fs.statSync(programPath).size;
check(
  'the kernel handed the host the image of the program INIT exec\'d',
  exec !== undefined && exec.path === '/bin/sh' && exec.moduleBytes === MODULE_BYTES,
  exec === undefined
    ? 'the host was never asked (INIT did not reach exec, or the host could not compile it)'
    : `path=${exec.path} bytes=${exec.moduleBytes} (image module is ${MODULE_BYTES})`
);
// The arguments came out of the frame INIT built: argv[0] is the path it exec'd, and it is
// what the module dispatches on to decide it is the shell. So this is where the exec frame's
// argv and the module's argv are the same argv. A failure to parse it would show as an E2BIG
// or as `program: malformed argv blob` on the console, not as a plausible-looking success.
check(
  'the exec carried argv, and the module was told to be the shell',
  exec !== undefined && exec.argv.length === 1 && exec.argv[0] === '/bin/sh',
  exec === undefined ? 'no exec' : `argv=${JSON.stringify(exec.argv)}`
);

// The swap: the slot is running the module's entry in a new address space, and the instance
// `minix_init` was is no longer the one the slot names. That is what "the old instance is
// gone" means here — the object survives because the harness reads the reports it left, but
// nothing will run it again: the dispatch loop drives `st.inst`, and `st.inst` is the module.
check(
  'the slot that called exec now runs the new module\'s entry',
  exec !== undefined &&
    init.entry === 'minix_program_main' &&
    init.prev !== undefined &&
    init.prev.entry === 'minix_init' &&
    init.inst !== init.prev.inst,
  exec === undefined
    ? 'no exec'
    : `entry=${init.entry} (was ${init.prev && init.prev.entry}), ` +
      `instance replaced=${init.prev !== undefined && init.inst !== init.prev.inst}`
);

// And the negative control that makes the two above mean something: the old image's *failure*
// path also continues in this slot, and it writes a line to say so. Its absence, plus the
// prompt appearing only after the swap, is what says the exec succeeded rather than the
// failed-exec path having been taken — and the prompt can only have come from the new image,
// because the abandoned one was left inside a syscall it will never be rewound into.
const sawExecFailed = timeline.some((l) => l.includes('init: exec failed'));
const promptAt = timeline.findIndex((l) => l.includes('# '));
check(
  'the shell\'s output came from the image the exec created',
  !sawExecFailed && exec !== undefined && promptAt >= exec.timelineAt,
  `execFailedLine=${sawExecFailed} promptAt=${promptAt} swapAt=${exec && exec.timelineAt}`
);

note(
  'what M7a step 2 establishes, and what it does not',
  'A process can now become another program: INIT execs `/bin/sh`, the request goes through PM ' +
    'and VFS, VFS resolves the path against the boot image and hands the kernel the path and ' +
    'the exec frame, and the kernel\'s wasm arm asks the host to instantiate a module into the ' +
    'slot — which is `hal::exec_module` in §7.2. The stdio INIT had set up survives the exec ' +
    'because exec keeps the process, so the shell opens nothing and reads the same console. ' +
    'What is not here: the module\'s *bytes* still come from the host\'s registry rather than ' +
    'from the file VFS found (step 4), and the exec target is named by a path the host maps ' +
    'by hand. What is also not modelled is the process a shell *runs* — an external command ' +
    'needs fork, and only builtins work (M7\'s other half).'
);

// ---------------------------------------- M7b step 5a: fork, from a program

// The chain §11 scopes: userland's `fork` is PM's table copy, VM's address-space clone, the
// kernel's `Proc` and the schedule that makes the child runnable — and on this port the clone
// itself is the host's, because an address space here is an instance's memory (§5.1). The module
// is the one `echo` and `/bin/sh` come from; what makes it the program that forks is `argv[0]`.
const forktest = procs.find((p) => p.spec.label === 'forktest');
// The program's fork, told apart from the shell's: 5b puts a second fork in the same run, and
// "a fork happened" would then pass on the wrong one. Which process forked is the subject here.
const forkRecord = forks.find((f) => f.parent === forktest.spec.slot) ?? null;
const forkedChild = forkRecord === null ? undefined : forkRecord.st;

check(
  'the kernel asked the host to clone the program, and the host made a copy',
  forkRecord !== null && forkedChild !== undefined && forkedChild.forkOf === forktest.spec.slot,
  forkRecord === null
    ? `the program (slot ${forktest.spec.slot}) never forked; ` +
      `forks=[${forks.map((f) => `${f.parent}->${f.child}`).join(', ')}]`
    : `child slot=${forkRecord.child} forkOf=${forkedChild.forkOf}`
);

check(
  'the clone became an instance of its own, in a slot the kernel chose',
  forkedChild !== undefined &&
    forkedChild.inst !== forktest.inst &&
    forkedChild.memory !== forktest.memory &&
    forkedChild.spec.slot === forkRecord.child,
  forkedChild === undefined
    ? 'no second instance was created'
    : `slot=${forkedChild.spec.slot} forkOf=${forkedChild.forkOf} ` +
      `instance is the parent's=${forkedChild.inst === forktest.inst}`
);

// Invariant 2 of the fork spike, asserted against the bytes rather than trusted: a copy that was
// short by a page would truncate whatever the parent had grown past the module's minimum, and the
// child would resume into a hole with nothing downstream reporting the truncation.
check(
  "the child's memory was the parent's, byte for byte, at the fork point",
  forkRecord !== null && forkRecord.exact,
  forkRecord === null
    ? 'no fork'
    : `copied ${forkRecord.parentBytes} bytes into a memory of ${forkRecord.childBytes}`
);

// What the two instances said, read by value. The child's pid is one it asked PM for itself
// (`getpid`) and the parent's is the one PM answered its `fork` with, so a match is two routes out
// of the same process table agreeing — and `fork=0` on the child's line against a pid on the
// parent's is the divergence itself, from the one call the two share.
const childLine = timeline.find((l) => l.includes('forktest: child '));
const parentLine = timeline.find((l) => l.includes('forktest: parent pid='));
const reapedLine = timeline.find((l) => l.includes('forktest: parent reaped '));
const childMatch =
  childLine === undefined ? null : /child pid=(\d+) fork=0$/.exec(childLine.trimEnd());
const parentMatch =
  parentLine === undefined ? null : /parent pid=(\d+) fork=(\d+)$/.exec(parentLine.trimEnd());
const reapedMatch =
  reapedLine === undefined ? null : /reaped pid=(\d+) status=(\d+)$/.exec(reapedLine.trimEnd());

check(
  'the child printed the 0 that fork returned to it, and named its own pid',
  childMatch !== null,
  childLine ?? '(the child printed nothing)'
);
check(
  "the pid the parent's fork returned is the child's own pid, and not the parent's",
  parentMatch !== null &&
    childMatch !== null &&
    parentMatch[2] === childMatch[1] &&
    parentMatch[1] !== childMatch[1],
  `parent: ${parentLine ?? '(nothing)'} | child: ${childLine ?? '(nothing)'}`
);
check(
  'the parent reaped the child with status 0, so PM answered a waiting parent',
  reapedMatch !== null &&
    childMatch !== null &&
    reapedMatch[1] === childMatch[1] &&
    Number(reapedMatch[2]) === 0,
  reapedLine ?? '(waitpid never returned)'
);

// Finding 39's fix, observed rather than asserted about internals: PM registers a process it did
// not create at the slot the *endpoint* names, and the pid it answers with is the one registration
// gives that slot (`slot + 1`). The program's slot is outside the kernel's `BOOT_IMAGE`, so the pid
// is only that number if PM learned about the process rather than finding it in a boot list — and
// the fork above is the rest of the proof, because a `PM_FORK` PM cannot place is dropped and the
// parent blocks forever instead of reaching these lines.
check(
  'the forking program is a process of its own, registered at the slot its endpoint names',
  parentMatch !== null && Number(parentMatch[1]) === forktest.spec.slot + 1,
  parentMatch === null
    ? '(the parent printed no pid line)'
    : `parent pid=${parentMatch[1]}, slot=${forktest.spec.slot} (expected ${forktest.spec.slot + 1})`
);
check(
  'each instance ended in its own exit, the child first',
  forkedChild !== undefined && forkedChild.exited === true && forktest.exited === true,
  forkedChild === undefined
    ? 'no child'
    : `child exited=${forkedChild.exited}, parent exited=${forktest.exited}`
);

note(
  'what M7b step 5a establishes, and what it does not',
  'A program can now fork: the kernel asked the host to clone the parent (hal::fork_process, ' +
    '§7.2), the child resumed inside the same syscall the parent is suspended in and saw 0 where ' +
    'the parent saw a pid, and the two instances agreed about who the child is. The parent reaping ' +
    'it is PM\'s first GETKSIG for a wasm process that died with a parent waiting. What is not ' +
    'here is what a shell does with fork: an exec in the child, which is 5b, and the repeated ' +
    'suspend/resume a loop of forks needs, which nothing in the spike covered. Nor is the ' +
    'address-space handle anything but a stand-in — VM records the child\'s endpoint where a CR3 ' +
    'would go (§5.3), so nothing that walks it can mean anything yet.'
);

// --------------------------------- M7b step 5b: the shell runs a command it cannot do itself

// The console script's first line is `/bin/echo ...`, which the shell cannot answer from a
// builtin: it builds a path, forks, and the child execs what the image carries there. So this is
// 5a's chain and M7a's exec on one line of a script, and every hop is a check elsewhere — what is
// checked here is that they *compose*, which is M3's title.
const shellFork = forks.find((f) => f.parent === init.spec.slot);
// The fork's own record of the child, not `procAt(slot)`: the forktest program forks later in
// the same run and the kernel may hand it this slot, so what lives at that slot at the end is
// not what the shell forked.
const shellChild = shellFork === undefined ? undefined : shellFork.st;
check(
  'the shell forked for the external command, and the child is an instance of its own',
  shellFork !== undefined &&
    shellChild !== undefined &&
    shellChild.forkOf === init.spec.slot &&
    shellChild.inst !== init.inst,
  shellFork === undefined
    ? `the shell (slot ${init.spec.slot}) never forked; forks=[${forks.map((f) => `${f.parent}->${f.child}`).join(', ')}]`
    : `child slot=${shellFork.child}`
);

// The path is the *kernel's* record of what the child asked to become — read out of its memory
// by `hostExecModule`, not supplied by this harness — and the byte count is the image builder's,
// so a match says the module the child became is the one the script's path names in the boot
// image. The path and `argv[0]` are both the command line's, so a shell that resolved something
// else would disagree here.
check(
  "the child exec'd the path the shell typed, from the image's copy of the module",
  shellChild !== undefined &&
    shellChild.exec !== undefined &&
    shellChild.exec.path === EXTERNAL_CMD &&
    shellChild.exec.argv[0] === EXTERNAL_CMD &&
    shellChild.exec.moduleBytes === MODULE_BYTES,
  shellChild === undefined || shellChild.exec === undefined
    ? 'the child never exec\'d'
    : `path=${shellChild.exec.path} argv=${JSON.stringify(shellChild.exec.argv)} ` +
      `bytes=${shellChild.exec.moduleBytes} (image module is ${MODULE_BYTES})`
);

// `echo` prints its arguments, so the line says three things: the exec'd image parsed the argv
// the shell passed, it reached the `echo` arm (a module that had not heard of `/bin/echo` would
// have refused with `no such command`), and its output reached the console — through the fds the
// shell inherited from INIT, since this process never opened anything.
const EXTERNAL_LINE = `kernel: ${EXTERNAL_ARGS.join(' ')}`;
const sawExternal = timeline.some((l) => l.trimEnd() === EXTERNAL_LINE);
check(
  "the external command's output reached the console, from the image the exec created",
  sawExternal &&
    shellChild !== undefined &&
    shellChild.exec !== undefined &&
    timeline.findIndex((l) => l.trimEnd() === EXTERNAL_LINE) >= shellChild.exec.timelineAt,
  `looked for ${JSON.stringify(EXTERNAL_LINE)} after the exec at line ` +
    `${shellChild && shellChild.exec && shellChild.exec.timelineAt}`
);

// And the reap. The claim is not just that the child died but that the shell was waiting for it:
// `init` is the slot the shell is in, and its own exit is the *last* thing it does, so a shell
// that never reaped would still be blocked in `waitpid` when the script reaches `exit` — the two
// together are what say the wait completed.
check(
  'the child exited, and the shell reaped it and went on to exit itself',
  shellChild !== undefined &&
    shellChild.exited === true &&
    init.exited === true &&
    init.tail.length > 0 &&
    init.tail[init.tail.length - 1].nr === EXIT,
  shellChild === undefined
    ? 'no child'
    : `child exited=${shellChild.exited}, shell exited=${init.exited}, ` +
      `shell tail=[${init.tail.map((t) => t.nr).join(',')}]`
);

note(
  'what M7b step 5b establishes',
  "M3's title is earned: the shell runs a command it cannot answer from a builtin. It resolves " +
    'the path, forks (PM\'s table, VM\'s clone — on this port the host\'s memory copy), the child ' +
    'execs `/bin/echo` (PM → VFS → the kernel → the host instantiating a module from the boot ' +
    'image), the command prints through the stdio INIT opened, the child exits, and the shell ' +
    'reaps it and reads its next line. The one thing a shell still cannot do is loop over ' +
    'commands efficiently: each fork clones the whole instance, which the spike measured at ' +
    '*memory size* per fork rather than stack depth.'
);

note(
  'what this establishes, and what it does not',
  'All three servers ran their own init and blocked in RECEIVE; RS registered its ' +
    'grant table and handed DS the public process table, which DS copied out of ' +
    "RS's own instance and mapped into its label table; DS then answered that " +
    'request and RS consumed the answer, moving DS out of `RS_INITIALIZING`; PM ' +
    'answered its own init request from its main loop and RS consumed that too; PM ' +
    'consumed the RS boot notification; and DS served a real client that announced ' +
    'itself with `rs_up`, reading the key out of the client\'s instance through the ' +
    'copy seam and addressing both replies back to the client. The control client — ' +
    'same protocol, same key, no announcement — is still refused, so the ' +
    'authorisation the label table provides is measured rather than assumed, and the ' +
    'announced client is refused when it claims to be initialised, because only RS ' +
    'decides who has been asked. What is not here is an init request for the other ' +
    'services this port starts: the `asked` table in `rs_server_main` grows a ' +
    'service at a time as its loop learns to answer, and the period/heartbeat and ' +
    'restart machinery that goes with a service that never answers is what a ' +
    'runtime-start path will still need.'
);

console.log('\nper-server syscall trace:');
for (const p of procs) {
  console.log(`  ${p.spec.label} (slot ${p.spec.slot}, ep ${p.spec.endpoint}):`);
  for (const t of p.trace) console.log(`    nr=${t.nr} a0=0x${t.a0.toString(16)}`);
  // The tail as well as the head: `trace` stops at `TRACE_LIMIT`, so for a process that made
  // more calls than that — every server, and anything that forks late — the head says nothing
  // about what it was doing when the run ended, which is the only question a stuck run asks.
  if (p.tail.length > 0) console.log(`    ... tail: ${p.tail.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join(', ')}`);
}

console.log('\nkernel console:');
for (const line of timeline) console.log(`  ${line}`);

console.log(
  `\ncross-process copies the kernel asked the host for (${copyLog.length}):`
);
if (copyLog.length === 0) {
  console.log('  (none - nothing has been delivered to these servers yet)');
}
for (const c of copyLog) {
  console.log(
    `  ${c.srcProc}:0x${c.srcAddr.toString(16)} -> ${c.dstProc}:0x${c.dstAddr.toString(16)}` +
      ` (${c.bytes} bytes) => ${c.result}`
  );
}

const failed = checks.filter((c) => !c.ok);
console.log(`\n${checks.length - failed.length}/${checks.length} checks passed`);
process.exit(failed.length === 0 ? 0 : 1);
