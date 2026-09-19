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
// The boot filesystem image, in the same place the QEMU builds write it. This
// harness does not build userland, so it reuses whichever image is on disk — any
// valid MinixFS image will do here, because what is under test is that the image
// reaches the RAM disk instance and that the device is sized from it.
const ramdiskImagePath = path.join(
  __dirname,
  '..',
  '..',
  'target',
  'images',
  'x86_64-pc-minix',
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
const SYSCALL_BUDGET = 2000;
const TRACE_LIMIT = 64;
// How many of the *latest* syscalls to keep. Small on purpose: the loop checks
// want the last one and the few before it.
const TAIL_LIMIT = 8;
let syscallsLeft = SYSCALL_BUDGET;
/// Which instance ran the budget out, if any. Named rather than booleans so the
/// failure report says who was spinning.
let exhaustedBy = null;

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

// ----------------------------------------------------------- kernel instance

const kernelModule = new WebAssembly.Module(fs.readFileSync(kernelPath));
const kernel = new WebAssembly.Instance(kernelModule, {
  env: {
    host_console_write: (b) => {
      consoleTag = 'kernel';
      emit(b);
    },
    host_console_read: () => -1,
    host_console_available: () => 0,
    host_cycles: () => BigInt((cycles += 1000)),
    host_halt: (code) => {
      haltCode = code;
    },
    host_copy_between: copyBetween,
  },
});

kernel.exports.minix_kernel_init();

// ---------------------------------------------------------- server instances

const serverModule = new WebAssembly.Module(fs.readFileSync(serverPath));

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
];

for (const s of specs) s.endpoint = kernel.exports.minix_make_endpoint(s.slot);

function makeServer(spec) {
  const memory = new WebAssembly.Memory({ initial: 256, maximum: 4096 });
  const st = {
    spec,
    memory,
    inst: null,
    started: false,
    exited: false,
    pending: null,
    blockedCount: 0,
    dataPtr: 0,
    scratch: 0,
    syscalls: 0,
    trace: [],
    // The *last* few syscalls, as well as the first `TRACE_LIMIT`. The checks that
    // ask "is it in its main loop?" are asking about the tail, but a head-only
    // trace answers with whichever server made the most calls before it looped:
    // VM queries the kernel about every process slot first (`vm_init_boot`), which
    // is 256 calls, so a 64-entry head never reaches its `RECEIVE`. The client
    // checks need the head, so both are kept.
    tail: [],
  };

  const imports = {
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
          return EINVAL;
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
          return BigInt(kernel.exports.minix_proc_retval(spec.slot));
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

  st.inst = new WebAssembly.Instance(serverModule, imports);
  // The module names its own scratch region; it is not derived from a linker
  // default. The 16-byte Asyncify struct goes at its start and the unwind stack
  // follows.
  st.scratch = st.inst.exports.asyncify_scratch_ptr();
  st.dataPtr = st.scratch;
  const bufStart = st.dataPtr + STRUCT_SIZE;
  writeU32(memory, st.dataPtr + 0, bufStart);
  writeU32(memory, st.dataPtr + 4, bufStart + BUF_SIZE);
  writeU32(memory, st.dataPtr + 8, bufStart);
  return st;
}

const procs = specs.map(makeServer);

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

const spawnFailures = specs.filter(
  (s) => kernel.exports.minix_proc_spawn(s.slot, s.endpoint) !== 0
);
check(
  'the kernel accepts every instance into its table',
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
  st.inst.exports[st.spec.entry]();
  const state = st.inst.exports.asyncify_get_state();
  if (state === STATE_UNWINDING) return;
  st.exited = true;
  kernel.exports.minix_proc_exit(st.spec.slot);
}

let steps = 0;
let converged = true;
for (;;) {
  const slot = kernel.exports.minix_step();
  if (slot === -1) break;
  // A safety net, not a bound: the exchange adds round trips, and a resume
  // consumes a step like a first run does. A runaway is caught by the syscall
  // budget instead, which names the instance.
  if (++steps > 256) {
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

// PM is the one that has something to do. `boot_init` leaves RS's notification
// pending on PM's privilege structure, and PM finds it on its first RECEIVE —
// then asks the kernel for pending signals, finds none, and goes back to waiting.
// That round trip is the chain starting to move, and it is visible only because
// the notification reached PM's own memory.
const pm = procs.find((p) => p.spec.label === 'pm');
// The three things this claims, as properties rather than as a sequence of steps.
// The sequence assertion it used to be held exactly while no *user* process existed;
// INIT now sends PM a PM_GETPID and then an exit to report, so PM's trace legitimately
// has more in it -- SENDNB to endpoint 10, and a second GETKSIG round with the
// `SYS_ENDKSIG` that closes it. Pinning the whole trace was asserting where the boot
// had got to rather than what the protocol is, which is finding 27's lesson.
const pmSteps = pm.trace.map((t) => [t.nr, t.a0]);
const pmFirstGetksig = pmSteps.findIndex(([nr, a0]) => nr === KERNEL_CALL && a0 === GETKSIG);
const pmRsAnswer = pmSteps.findIndex(([nr, a0]) => nr === SENDREC && a0 === rs.spec.endpoint);
const pmLastStep = pmSteps[pmSteps.length - 1];
check(
  'PM consumed the boot notification, answered RS, and returned to receiving',
  pmSteps.length > 0 &&
    pmSteps[0][0] === RECEIVE &&
    pmSteps[0][1] === ANY &&
    pmFirstGetksig > 0 &&
    pmRsAnswer > pmFirstGetksig &&
    pmLastStep[0] === RECEIVE &&
    pmLastStep[1] === ANY,
  pm.trace.map((t) => `nr=${t.nr} a0=0x${t.a0.toString(16)}`).join('; ')
);
// PM's copies, taken by *who they involve* rather than by position: RS's init now
// asks the kernel for two copies of its own (the `SYS_SETGRANT` message and its
// reply) before PM runs, so a prefix of the log is no longer PM's. The first three
// are PM's own exchange with the kernel: the notification arriving, PM's
// `SYS_GETKSIG` message being read *out of PM's memory*, and the reply going back.
// Before the kernel-call fix the middle copy did not exist — the kernel read its
// own memory in place of PM's message and dispatched on that.
const pmCopies = copyLog.filter((c) => c.srcProc === 0 || c.dstProc === 0);
const pmNotifyCopies = pmCopies.slice(0, 3);
check(
  'the notification, the kernel-call message and its reply all crossed the seam',
  pmNotifyCopies.length === 3 &&
    pmCopies.every((c) => c.result === 0) &&
    pmNotifyCopies[0].srcProc < 0 &&
    pmNotifyCopies[0].dstProc === 0 &&
    pmNotifyCopies[1].srcProc === 0 &&
    pmNotifyCopies[1].dstProc < 0 &&
    pmNotifyCopies[2].srcProc < 0 &&
    pmNotifyCopies[2].dstProc === 0,
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
const sawStop = timeline.some((l) => l.includes('no console device or shell yet'));
check(
  "INIT's console output reached the harness through the kernel",
  sawBanner && sawStop,
  `banner=${sawBanner} closingLine=${sawStop}`
);
const pidLine = timeline.find((l) => l.includes('init: pid='));
check(
  'INIT reached PM over SENDREC and PM answered with its pid',
  pidLine !== undefined && pidLine.endsWith('init: pid=11'),
  `line=${JSON.stringify(pidLine)} (expected one ending "init: pid=11")`
);

// And it ended, by exiting rather than by trapping: the last syscall it made was
// EXIT, which is the only thing that lets the harness tell the two traps apart.
const initTail = init.tail.map((t) => t.nr);
check(
  'INIT exited through the exit syscall',
  init.exited === true && initTail[initTail.length - 1] === EXIT,
  `exited=${init.exited} tail=[${initTail.join(',')}]`
);

note(
  'what M3d establishes, and what M3 still owes',
  'INIT is the first process on this port that is not a server: the kernel links it ' +
    'to the shared USER privilege slot, its console output leaves through the ' +
    'kernel\'s copy seam, it reaches PM with a real SENDREC, and it exits through ' +
    'SYS_EXIT, which is what notifies PM of a death. What M3 still owes is the TTY ' +
    'half: `userland::init` cannot run to its end until `/dev/console` exists (a tty ' +
    'server behind VFS\'s device layer) and `/bin/sh` exists to be exec\'d, and its ' +
    'no-console path is a spin with no syscall in it, which on this target hangs the ' +
    'host instead of failing (finding 12).'
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
