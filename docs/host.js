// The wasm host engine: the kernel, the servers and the programs, driven from JavaScript.
//
// This is the browser's half of the port, and it is deliberately free of any DOM or Node
// dependency so the same code can be driven from a page (`page.js`) and from a script
// (`run.js`, which is what the project's own verification uses).
//
// It is not the *only* engine in the tree: `tools/wasm-servers/boot.cjs` drives the same system to
// assert the same facts, and the two share a design rather than a file. This one exists because an
// interactive session needs something a check harness does not — the ability to stop a running
// guest, hand control back to the browser, and resume it later.
//
// The layers are the port's, in the order they are reached:
//
//   * **The console** — bytes in a queue the front end fills from the keyboard, bytes out to
//     whatever draws them. The kernel drains the queue into its serial ring and the tty pulls
//     from there, so this is the port's console seen from outside.
//   * **The copy seam** (§5.1) — the host is this port's page table. The kernel decides what
//     moves; `host_copy_between` moves it, and refuses what it cannot reach instead of trapping.
//   * **exec** (§7.2) — a program is a wasm module, so "install the new image" is module
//     instantiation, and only the host can instantiate. The bytes come from the boot image the
//     kernel read, which is why this needs no program registry of its own.
//   * **fork** (§12 risk 1) — an address space is an instance's linear memory, so a fork is a
//     byte copy of the parent's memory plus a copy of the host-side record. The invariants are
//     in `cloneProcess`, and `tools/fork-spike/` is their authority.
//   * **The dispatch loop** — `minix_step()` names a slot, `run()` enters it, and Asyncify is how
//     a guest that blocks (or that the host stops) gives control back.
//
// # Why the host stops the guest, and why that is not an optimisation
//
// A browser cannot let the guest run to quiescence the way a script can: a keystroke has to reach
// the guest while it is running, and an idle prompt must not pin the tab.
//
// The second is not hypothetical. The shell's `read_line` retries `read(0)` in user mode when
// there is nothing to read, and on this port the tty's own `do_read` does the same, so an idle
// console is a *spin*: it never blocks and never returns to the host's loop. That is
// `PORTING_PLAN.md` finding 32, still open. `pump()` therefore ends a slice after `maxSyscalls`,
// unwinding the instance through Asyncify exactly as a blocked syscall does, and the front end
// decides when to resume it. `sliceWasSpinOnly()` answers whether the slice did nothing but retry
// the console read, which is the observable form of "the guest is waiting for you" and what lets
// a front end park it instead of letting it spin. The guest-side fix — a console read that blocks,
// and a host input event to wake it — is finding 32's remaining work; the park is a workaround
// for its absence rather than a design.

const WASM_PAGE = 65536;

/// The kernel's window onto the boot filesystem image, from `arch_common::com`. On wasm this is
/// exactly `MAX_USER_ADDRESS`: an instance's memory holds the process and nothing else, so there
/// is no high half for a device window and the image sits immediately above the process's own VA
/// range.
const RAMDISK_IMAGE_VA = 0x1000000;

// Syscall numbers, from `minix-rt`. Only the shapes the host has to recognise are named.
const NR_READ = 2;
const NR_EXIT = 0;
const NR_THREAD_YIELD = 59;
const EAGAIN = -11;
const EFAULT = -14;
/// No such device. What `host_block_*` answers when the host has no device attached, which is how
/// `virtio_blk`'s probe learns there is nothing there and MFS's root falls back to the ramdisk.
const ENODEV = -19;
const EINVAL = -22;
const ENOEXEC = -8;
const ENOMEM = -12;

/// Asyncify's state while an instance is unwinding, which is how `run()` tells a guest that gave
/// control back from one that finished.
const STATE_UNWINDING = 1;

const STRUCT_SIZE = 16;
const BUF_SIZE = 65536;

/// Field offsets of `ExecModuleRequest`, which asserts its own layout on the Rust side.
const EXEC_REQ_IMAGE_PROC = 0;
const EXEC_REQ_IMAGE_ADDR = 4;
const EXEC_REQ_IMAGE_LEN = 8;
const EXEC_REQ_PATH_ADDR = 12;
const EXEC_REQ_ARGV_ADDR = 16;
const EXEC_REQ_ARGC = 20;

/// The system this port boots, in the order the kernel's `BOOT_IMAGE` gives it. The slots and
/// entry points are the contract: a server is the same module under a different entry, and the
/// slot is what the kernel's `Proc` table, PM's mproc table and VFS's fproc table all index by.
///
/// The order matters twice over. VM comes before MFS because every process that calls `brk()`
/// depends on it and MFS's allocator runs during init; VFS comes last because its init calls
/// `mount_root`, which asks MFS for the superblock; and the tty comes after VFS because its init
/// registers the console with devman, which VFS has to have mounted.
///
/// `fb` is early among the servers because it is the only one with nothing to talk to at boot: it
/// asks the host for its mode, paints its surface and waits for a client to open `/dev/fb`
/// (M5a). Its slot is where the boot image's device map already points major 19, which is why a
/// boot without it is a `/dev/fb` that nothing answers.
///
/// `wserver` follows `fb` and precedes `tty`, and that order is load-bearing in both directions
/// (M5b): the compositor's frames reach the display through the fb driver, so the driver has to be
/// alive with its surface painted before the first frame arrives; and the console's window is
/// created at tty's init, so the window server has to be receiving requests by then. A `tty` before
/// `wserver` would leave the console's create blocked on a peer that has not started.
///
/// `input` sits between `fb` and `wserver`, which is the order the kernel's own `BOOT_IMAGE` gives
/// (M5c). The direction that matters is the second one: `wserver` registers itself as the input
/// server's consumer while it attaches, so the server it registers with has to be there — and on
/// this port the input server is not optional, because the host's own records are the only keyboard
/// and pointer the desktop has.
export const SYSTEM_SPECS = [
  { slot: 6, entry: 'minix_server_ds', label: 'ds' },
  { slot: 2, entry: 'minix_server_rs', label: 'rs' },
  { slot: 0, entry: 'minix_server_pm', label: 'pm' },
  { slot: 11, entry: 'minix_server_ramdisk', label: 'ramdisk' },
  { slot: 8, entry: 'minix_server_vm', label: 'vm' },
  { slot: 7, entry: 'minix_server_mfs', label: 'mfs' },
  { slot: 12, entry: 'minix_server_virtio_blk', label: 'virtio_blk' },
  { slot: 15, entry: 'minix_server_devman', label: 'devman' },
  { slot: 1, entry: 'minix_server_vfs', label: 'vfs' },
  { slot: 16, entry: 'minix_server_fb', label: 'fb' },
  { slot: 17, entry: 'minix_server_input', label: 'input' },
  { slot: 18, entry: 'minix_server_wserver', label: 'wserver' },
  { slot: 5, entry: 'minix_server_tty', label: 'tty' },
  { slot: 10, entry: 'minix_init', label: 'init' },
];

/// Thrown when the guest has spent the syscall budget.
///
/// An error *return* is something the guest decides what to do with, and a guest in a retry loop
/// may simply retry, so a budget that only returns an errno can be ignored. Throwing unwinds out
/// of the guest's dispatch to the loop that owns it, which can then stop and say who was
/// spinning. That is `PORTING_PLAN.md` finding 12.
class BudgetExhausted extends Error {}

const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder();

const utf8Bytes = (s) => utf8Encoder.encode(s).length;
const utf8String = (bytes) => utf8Decoder.decode(bytes);

const writeU32 = (memory, addr, v) => new DataView(memory.buffer).setUint32(addr, v, true);

/// Read a NUL-terminated string from an instance's memory, bounded: the string comes from a
/// process's address space, so the scan has to end somewhere even when the terminator does not.
function cString(mem, addr, max) {
  const bytes = new Uint8Array(mem.buffer);
  let end = addr;
  const limit = Math.min(addr + max, bytes.length);
  while (end < limit && bytes[end] !== 0) end += 1;
  return utf8String(bytes.subarray(addr, end));
}

function equalBytes(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/// Bring the wasm system up and hand back the host object.
///
/// The artifacts are the ones `tools/wasm-servers/build.sh` produces: the kernel instance, the
/// servers as one module, and the boot filesystem image. `sink` receives what the guest writes to
/// the console (`write`, one byte at a time) and the host's own reports (`note`) — the front end
/// decides what to do with either.
///
/// `store` is where the block device's bytes live between runs, and the store contract — the four
/// synchronous calls, and what `imageId` guards — is documented in `store.js`.

export function createHost({
  kernel: kernelBytes,
  servers: serversBytes,
  image: imageBytes,
  sink = { write() {}, note() {} },
  /// Where the block device's contents live between runs, or omitted for a diskless boot. Without
  /// one there is no device to attach, `virtio_blk` finds nothing, and MFS mounts the root from
  /// the ramdisk — the same path a machine with an empty drive takes.
  store = null,
  /// The display, or omitted for a headless run: `{width, height, present(bytes)}`, where `bytes`
  /// is one frame in the layout the guest's `fb` driver describes (XRGB8888, rows back to back).
  /// The mode is the host's — a canvas is the size the page made it, the way a panel is the size
  /// the hardware fixed — and `present` is where a frame goes: a canvas in a page, a recorder in
  /// a check. `display.js` is the canvas one; `run.js` and `boot.cjs` hand over recorders.
  display = null,
  specs = SYSTEM_SPECS,
  /// The host's lever against a guest that keeps making syscalls without getting anywhere
  /// (finding 12). Generous: a boot with a shell, a fork and an exec costs a five-figure number,
  /// and a *spinning* guest is caught by the slice instead of by this.
  syscallBudget = 1000000,
  /// How many syscalls one `pump()` may run before it unwinds and hands control back. Small
  /// enough that a front end stays responsive between slices, large enough that a slice is real
  /// work rather than overhead: a syscall here costs a few microseconds, so a slice is a handful
  /// of milliseconds.
  sliceSyscalls = 400,
} = {}) {
  const note = (name, detail) => sink.note(name, detail);

  // ------------------------------------------------------------------ the device

  /// What the host's block device has been asked for. The checks read this rather than asking the
  /// guest: "the root came from the disk" is a claim about *these* numbers — a mount that read
  /// sectors and a filesystem that wrote them back.
  const deviceStats = { reads: 0, writes: 0, bytesRead: 0, bytesWritten: 0, attached: false };

  /// Identify an image by its size and contents, so a store can be told from an image it was not
  /// made from. Not a hash for security — a mismatch has to be *detected*, and a fingerprint the
  /// build cannot accidentally reproduce is enough.
  const imageIdOf = (bytes) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < bytes.length; i += 1) {
      h ^= bytes[i];
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return `${bytes.length}:${h.toString(16)}`;
  };

  /// The block device, or null when there is none.
  ///
  /// `capacity` is the boot image's length: the image is this disk's *initial contents*, so a fresh
  /// store boots an installed system and every later run sees what the previous one wrote. There is
  /// no separate "install" step to get wrong, and no way for the guest to see a disk that is
  /// neither.
  const device = (() => {
    if (store === null) return null;
    const imageId = imageIdOf(imageBytes);
    if (store.imageId !== null && store.imageId !== imageId) {
      note(
        'the block device was not attached: its contents were made from a different image',
        `store has ${store.imageId}, image is ${imageId} — delete the store to start over ` +
          `(the page offers a control for that), or the filesystem on the disk would be two ` +
          `filesystems mixed`
      );
      return null;
    }
    if (store.imageId === null) store.setImageId(imageId);
    deviceStats.attached = true;
    return {
      capacity: imageBytes.length,
      /// Copy `bytes` from the device at `offset` into `memory` at `dstAddr`. Short at the end of
      /// the device, as a block device is.
      readInto(memory, offset, dstAddr, bytes) {
        if (offset >= imageBytes.length) return 0;
        const want = Math.min(bytes, imageBytes.length - offset);
        const chunk = store.read(offset, want);
        new Uint8Array(memory.buffer, dstAddr, want).set(chunk);
        deviceStats.reads += 1;
        deviceStats.bytesRead += want;
        return want;
      },
      /// Copy `bytes` into the device from `memory` at `srcAddr`.
      writeFrom(memory, offset, srcAddr, bytes) {
        if (offset >= imageBytes.length) return 0;
        const want = Math.min(bytes, imageBytes.length - offset);
        store.write(offset, new Uint8Array(memory.buffer, srcAddr, want));
        deviceStats.writes += 1;
        deviceStats.bytesWritten += want;
        return want;
      },
    };
  })();

  // ------------------------------------------------------------------- the display

  /// What the display has been asked for, the way `deviceStats` records the device: a check reads
  /// these rather than asking the guest, because "the guest's frame reached the display" is a
  /// claim about *these* numbers.
  const displayStats = {
    frames: 0,
    bytes: 0,
    mode: display === null ? [0, 0] : [display.width, display.height],
    attached: display !== null,
  };

  /// Hand one frame of an instance's memory to the display.
  ///
  /// A frame that is not the mode's size is refused rather than shown: the guest's driver sends
  /// exactly the mode it was given, so any other number means the two sides disagree about the
  /// mode — and a partial picture is the shape of bug that looks like a drawing error.
  const presentFrame = (memory, srcAddr, bytes) => {
    if (bytes !== display.width * display.height * 4) return EINVAL;
    display.present(new Uint8Array(memory.buffer, srcAddr, bytes));
    displayStats.frames += 1;
    displayStats.bytes += bytes;
    return 0;
  };

  // ---------------------------------------------------------------------- state

  /// One record per slot the kernel knows a process by: the host's copy of what the kernel's
  /// `Proc` holds — which instance, which memory, which entry, what it is suspended in. Fork's
  /// third invariant is that memory is not the whole process, which is why this exists as a thing
  /// to copy rather than as a lookup.
  const procs = [];
  const bySlot = new Map();
  const procAt = (slot) => bySlot.get(slot);

  /// Every fork the kernel asked for: who, into which slot, and the child's own record — not a
  /// lookup by slot, because a slot is a place and the process that lived there can be gone.
  ///
  /// `parentBytes` is what the clone cost, and `cloneMs` how long it took: the copy is the whole of
  /// what a fork costs here (a stack is not what is being duplicated) and it does not pass through
  /// `copyBetween`, so neither of the totals above would show it.
  const forks = [];

  /// Cross-process copies the kernel asked for, capped. Kept because a copy that returned
  /// `EFAULT` is otherwise invisible from both sides: the kernel sees an errno and the two
  /// processes see nothing at all.
  const copyLog = [];
  const COPY_LOG_LIMIT = 256;

  /// Every copy the kernel has asked for, totalled rather than kept.
  ///
  /// `copyLog` is a ring, because it answers "why did this one fail" and not "how much moved".
  /// What a command costs is the second question: a fork clones the whole parent instance, so one
  /// copy of `parentBytes` dominates a command's traffic and a front end has no other way to see
  /// either the count or the size.
  let copies = 0;
  let bytesCopied = 0;

  /// Bytes the front end has typed and the guest has not consumed. The kernel drains these into
  /// its serial ring; an empty queue answers `-1`, which is what makes an idle console a retry
  /// loop rather than a block (finding 32).
  const queue = [];
  const console_ = {
    queue,
    push(bytes) {
      const b = typeof bytes === 'string' ? utf8Encoder.encode(bytes) : bytes;
      for (const byte of b) queue.push(byte & 0xff);
    },
    get pending() {
      return queue.length;
    },
  };

  /// HID usage page of the keyboard, and the two interrupt lines the input server registers (M5c).
  ///
  /// A keyboard is IRQ 1 and a pointing device is IRQ 12 wherever they are attached, and this port's
  /// host is no exception: the line *is* the device's identity as far as the notifying side is
  /// concerned, which is why the host names one rather than the guest inferring it from the record.
  const KEY_PAGE = 0x0007;
  const IRQ_KEYBOARD = 1;
  const IRQ_POINTER = 12;

  /// Input records the front end has produced and the guest has not consumed (M5c).
  ///
  /// Records rather than bytes, because a record is what the guest's input server queues and what
  /// its consumer routes: a HID usage page, a usage, and a value. The host holds them for the reason
  /// it holds the console's bytes — the *guest* pulls — and the difference between the two queues is
  /// why they are separate: the console's is drained by the kernel on the process's next read, while
  /// this one is drained by a driver that has to be *woken*, which is what `push` does and `enqueue`
  /// deliberately does not.
  const inputQueue = [];
  /// Whether the host has *announced* something the guest has not taken yet — a raised line whose
  /// driver has not drained the queue since. It is what `sliceWasSpinOnly` cannot see: at the
  /// prompt the shell is the only instance the dispatcher reaches (it never blocks, so it stays at
  /// the head of a run queue this port rotates only when a syscall ends), and a parked front end
  /// would therefore never let the input server have the slice that a wake just made it deserve.
  let announced = false;

  /// Queue one input record and raise the line its driver registered.
  ///
  /// The record goes in *before* the interrupt: the guest's drain runs when the notification wakes
  /// it, so the other order is a drain that finds nothing, and an event that waits for the next
  /// wake — which is the wedge this milestone exists to avoid.
  function pushInput(page, code, value) {
    inputQueue.push({ page, code, value });
    announced = true;
    kernel.exports.minix_kernel_irq(page === KEY_PAGE ? IRQ_KEYBOARD : IRQ_POINTER);
  }

  const input_ = {
    queue: inputQueue,
    /// One event the way a front end produces them: queued and announced.
    push: pushInput,
    /// Queued but *not* announced — the control for the wake, because nothing else in the guest
    /// looks at this queue. A record held this way is one the guest never takes, and a check that
    /// cannot tell that from a delivered event is not checking the wake at all.
    enqueue(page, code, value) {
      inputQueue.push({ page, code, value });
    },
    /// Work the guest has been *told* about and has not taken: the front end's answer to "is there
    /// anything to wake it for", the way `console.pending` is for bytes. A record merely held here
    /// is not work — no one was told — which is what keeps the control below from parking the loop.
    get pending() {
      return announced ? inputQueue.length : 0;
    },
  };

  /// What the current slice did. `used` counts executed syscalls; `spin` counts those that were
  /// a console read with nothing to read or the `thread_yield` that follows one.
  let slice = { used: 0, spin: 0, limit: sliceSyscalls };

  let steps = 0;
  let syscallsLeft = syscallBudget;
  let exhaustedBy = null;
  let haltCode = null;
  let cycles = 0;

  const memoryFor = (proc) => {
    if (proc < 0) return kernel.exports.memory;
    const st = procAt(proc);
    return st === undefined ? null : st.memory;
  };

  function copyBetween(srcProc, srcAddr, dstProc, dstAddr, bytes) {
    const src = memoryFor(srcProc);
    const dst = memoryFor(dstProc);
    let result = 0;
    if (src === null || dst === null) result = EFAULT;
    else if (srcAddr < 0 || dstAddr < 0 || bytes < 0) result = EFAULT;
    else if (srcAddr + bytes > src.buffer.byteLength) result = EFAULT;
    else if (dstAddr + bytes > dst.buffer.byteLength) result = EFAULT;
    else new Uint8Array(dst.buffer).set(new Uint8Array(src.buffer, srcAddr, bytes), dstAddr);
    if (copyLog.length < COPY_LOG_LIMIT) {
      copyLog.push({ srcProc, srcAddr, dstProc, dstAddr, bytes, result });
    }
    copies += 1;
    if (result === 0) bytesCopied += bytes;
    return result;
  }

  // ------------------------------------------------------------------------- exec

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
    const st = procAt(slot);
    if (st === undefined || st.inst === null) return EINVAL;

    // Where and how big, without a copy: the bytes stay in the process that read them, and both
    // bounds are the engine's — a range past the end of the memory throws here.
    const image = new Uint8Array(imageMemory.buffer, imageAddr, imageLen);
    const path = cString(imageMemory, pathAddr, 256);
    const argv = [];
    let at = argvAddr;
    for (let i = 0; i < argc; i += 1) {
      const arg = cString(kernelMemory, at, 4096);
      argv.push(arg);
      at += utf8Bytes(arg) + 1;
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
    // path chose the module and `argv[0]` chooses the program inside it, so the entry is a
    // convention rather than a lookup — but it does have to be there.
    const entry = 'minix_program_main';
    if (!WebAssembly.Module.exports(module).some((e) => e.name === entry && e.kind === 'function')) {
      note('the module has no program entry', `path=${JSON.stringify(path)}: expected ${entry}`);
      return ENOEXEC;
    }

    try {
      st.prev = { inst: st.inst, memory: st.memory, entry: st.entry };
      instantiate(st, module, argv, entry);
      st.exited = false;
      st.exec = { path, argv, moduleBytes: imageLen };
      return 0;
    } catch (e) {
      note('instantiating the exec target module failed', `${path}: ${e}`);
      return ENOMEM;
    }
  }

  // ------------------------------------------------------------------------- fork

  /// Clone the process in `parentSlot` into `childSlot`.
  ///
  /// Called from the kernel's fork arm, which is the only layer that knows a fork happened — PM
  /// drove it and VM chose the slot, and neither can copy a memory it does not own.
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
    // A slot whose process has exited is a free slot, and the kernel is entitled to hand it to the
    // next fork: its `Proc` went SLOT_FREE when the process died and PM's mproc went with it. The
    // host has to reach the same conclusion, and the *record* is what moves — dropping the dead
    // one keeps one entry per live slot, which is what `procAt` requires. The fork record below
    // carries the child's own record, so "the child of this fork" still names the process it
    // means after the slot has been reused.
    if (occupant !== undefined) {
      procs.splice(procs.indexOf(occupant), 1);
      bySlot.delete(childSlot);
    }

    let child;
    const cloneStart = performance.now();
    try {
      child = cloneProcess(parent, childSlot);
    } catch (e) {
      note('cloning a process for fork failed', `${parentSlot} -> ${childSlot}: ${e}`);
      return ENOMEM;
    }
    const cloneMs = performance.now() - cloneStart;

    // Invariant 2, asserted rather than trusted: over the parent's whole length the child's
    // memory is the parent's, byte for byte. A copy short by a page would truncate whatever the
    // parent had grown past the module's minimum and nothing downstream would report it — the
    // child would simply resume into a hole.
    const parentBytes = parent.memory.buffer.byteLength;
    const childBytes = child.memory.buffer.byteLength;
    const exact =
      childBytes >= parentBytes &&
      equalBytes(
        new Uint8Array(child.memory.buffer, 0, parentBytes),
        new Uint8Array(parent.memory.buffer, 0, parentBytes)
      );

    procs.push(child);
    bySlot.set(childSlot, child);
    forks.push({
      parent: parentSlot,
      child: childSlot,
      st: child,
      exact,
      parentBytes,
      childBytes,
      cloneMs,
    });
    return 0;
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
  /// 3. The host-side record is duplicated, not just the memory — memory is not the whole
  ///    process. In particular the child inherits `pending`: it is suspended inside the same
  ///    syscall the parent is, and that is what makes its next dispatch a *rewind* rather than a
  ///    start.
  /// 4. The divergent return values are not written here at all. The resume path returns
  ///    `minix_proc_retval(slot)`, which the kernel set to 0 for the child, and PM's reply
  ///    decides the parent's. Nothing about a fork is asymmetric except which of those arrives.
  function cloneProcess(parent, childSlot) {
    const child = makeServer({
      slot: childSlot,
      entry: parent.entry,
      label: `${parent.spec.label}+F`,
      module: parent.module,
    });

    // (1) and (2): after instantiation, and the parent's size.
    const parentPages = parent.memory.buffer.byteLength / WASM_PAGE;
    const childPages = child.memory.buffer.byteLength / WASM_PAGE;
    if (parentPages > childPages) child.memory.grow(parentPages - childPages);
    new Uint8Array(child.memory.buffer, 0, parent.memory.buffer.byteLength).set(
      new Uint8Array(parent.memory.buffer)
    );

    // (3): the record. Shallow for the syscall pair, which nothing mutates in place, and deep for
    // the histories, which the child goes on appending to.
    child.pending = parent.pending === null ? null : { ...parent.pending };
    child.entryArgs = [...parent.entryArgs];
    child.tail = parent.tail.map((t) => ({ ...t }));
    child.syscalls = parent.syscalls;
    child.forkOf = parent.spec.slot;
    return child;
  }

  // ------------------------------------------------------------ instance plumbing

  /// Give a slot an instance to run: a fresh memory, the imports, the module's entry, and its
  /// arguments. Called once per slot at boot and again by exec, which is the whole reason it is a
  /// function of the *slot*: exec replaces what a slot runs without changing which slot it is,
  /// and without the kernel knowing the host did it.
  function instantiate(st, module, argv, entry) {
    st.memory = new WebAssembly.Memory({ initial: 256, maximum: 4096 });
    st.inst = new WebAssembly.Instance(module, makeImports(st));
    // The module names its own scratch region; it is not derived from a linker default. The
    // 16-byte Asyncify struct goes at its start and the unwind stack follows.
    st.scratch = st.inst.exports.asyncify_scratch_ptr();
    st.dataPtr = st.scratch;
    const bufStart = st.dataPtr + STRUCT_SIZE;
    writeU32(st.memory, st.dataPtr + 0, bufStart);
    writeU32(st.memory, st.dataPtr + 4, bufStart + BUF_SIZE);
    writeU32(st.memory, st.dataPtr + 8, bufStart);
    // The entry is the *module's*: `minix_init` and `minix_program_main` belong to different
    // modules, and which one a slot runs is what changes at exec.
    st.entry = entry ?? st.spec.entry;
    // A spec names a function the module has to export, and the two are written in different
    // places — one in this file, the other in `crates/wasm-servers`. A module that does not export
    // it is an artifact set from before that spec existed (the page fetches its own staged copies,
    // which is the usual way this happens), and the whole run is one TypeError deep inside a
    // slot otherwise. Said here instead, where the entry's name and its fix can be named.
    if (typeof st.inst.exports[st.entry] !== 'function') {
      throw new Error(
        `the loaded module does not export ${st.entry} (${st.spec.label}, slot ${st.spec.slot}): ` +
          `loaded artifacts are kernel ${kernelBytes.length} B, servers ${serversBytes.length} B, ` +
          `image ${imageBytes.length} B — if those are not the build's numbers the artifacts are ` +
          `stale; rebuild with sh tools/wasm-browser/build.sh`
      );
    }
    st.module = module;
    st.entryArgs = [];
    if (argv !== undefined) st.entryArgs = [argv.length, writeArgv(st, argv) + 4];
    // Whatever the replaced image was waiting on went with it: an exec does not return to the
    // syscall it replaced.
    st.pending = null;
  }

  /// Write argv into the instance's argv area, in the layout the module declares: `argc`, then a
  /// pointer array, then the NUL-terminated strings.
  ///
  /// The offsets are the module's, not the host's — the area's address comes from
  /// `argv_area_ptr()`, which is the only way a static inside a wasm module can be named.
  function writeArgv(st, argv) {
    const base = st.inst.exports.argv_area_ptr();
    const view = new DataView(st.memory.buffer);
    view.setUint32(base, argv.length, true);
    let at = base + 4 + 4 * argv.length;
    argv.forEach((arg, i) => {
      view.setUint32(base + 4 + 4 * i, at, true);
      const bytes = utf8Encoder.encode(arg);
      new Uint8Array(st.memory.buffer, at, bytes.length).set(bytes);
      // Wasm memory starts zeroed and nothing has written here, so the byte after each string is
      // already the terminator the entry scans for.
      at += bytes.length + 1;
    });
    return base;
  }

  function makeServer(spec) {
    const st = {
      spec,
      memory: null,
      inst: null,
      module: null,
      exited: false,
      pending: null,
      dataPtr: 0,
      scratch: 0,
      syscalls: 0,
      entry: spec.entry,
      entryArgs: [],
      prev: undefined,
      forkOf: undefined,
      exec: undefined,
      trace: [],
      tail: [],
    };
    instantiate(st, spec.module ?? serverModule, spec.argv);
    return st;
  }

  /// Build the import object an instance of this slot gets.
  ///
  /// A function of the *slot* rather than of the instance, because exec replaces a slot's
  /// instance and the replacement has to talk to the kernel through the same gate, in the same
  /// slot's name.
  function makeImports(st) {
    const memory = st.memory;
    const spec = st.spec;

    return {
      env: {
        memory,
        // The host is the clock: `performance.now()` in a page, a counter here. Every read is a
        // step forward, which is what makes a monotonic clock testable rather than timed.
        host_cycles: () => {
          cycles += 1000;
          return BigInt(cycles);
        },
        host_console_write: (b) => sink.write(b & 0xff),
        host_console_read: () => (queue.length === 0 ? -1 : queue.shift()),
        host_console_available: () => queue.length,
        host_halt: (code) => {
          haltCode = code;
        },
        // The kernel's copy seam and its exec and fork arms. All three are the host's because they
        // name two address spaces or none at all, and only the host owns every instance's memory.
        host_copy_between: copyBetween,
        host_exec_module: hostExecModule,
        host_fork_process: hostForkProcess,
        // The block device (M4). Three calls, and the driver above them is the same one the
        // hardware arches run: `virtio_blk`'s `probe` asks the capacity instead of scanning a bus,
        // and its `transfer` moves the bytes here instead of through a virtqueue. What stays the
        // guest's is everything about the protocol — who opens the device, what a sector is, and
        // which driver serves the root.
        host_block_capacity: () => BigInt(device === null ? 0 : device.capacity),
        host_block_read: (offset, bufAddr, bytes) =>
          device === null ? ENODEV : device.readInto(memory, Number(offset), bufAddr, bytes),
        host_block_write: (offset, srcAddr, bytes) =>
          device === null ? ENODEV : device.writeFrom(memory, Number(offset), srcAddr, bytes),
        // The display (M5a). Same division as the block device: the guest's `fb` driver owns the
        // surface, the mode it was given, and the protocol above it, while the host owns the two
        // things that are not the guest's — what size the display is and where the pixels go.
        host_fb_geometry: () =>
          BigInt(display === null ? 0 : display.width * 2 ** 32 + display.height),
        host_fb_present: (srcAddr, bytes) =>
          display === null ? ENODEV : presentFrame(memory, srcAddr, bytes),
        // The input queue (M5c). The same contract as the console's read, one record at a time: the
        // guest drains until it is told there is nothing, which is what makes a drain cheap. What is
        // different is where the *answer* comes from — the input server's IRQ hook, which `push`
        // raises through the kernel — because a driver blocked in RECEIVE cannot poll.
        //
        // The record is eight bytes: HID usage page and usage as little-endian u16, then the value as
        // a little-endian i32, which is the positive-and-negative form `arch_wasm32`'s import
        // documents and decodes.
        host_input_read: (outAddr) => {
          if (inputQueue.length === 0) {
            // The guest asked and there was nothing: whatever was announced has been taken, and the
            // front end is free to park again (see `input.pending`).
            announced = false;
            return -1;
          }
          const ev = inputQueue.shift();
          const view = new DataView(memory.buffer);
          view.setUint16(outAddr, ev.page, true);
          view.setUint16(outAddr + 2, ev.code, true);
          view.setInt32(outAddr + 4, ev.value, true);
          return 8;
        },
        // All six argument registers are named and forwarded, not only the two the
        // message-passing syscalls use: the kernel's dispatcher hands `args` straight to the
        // handler, and a syscall whose count arrives as a literal zero is a transfer that reports
        // success and moves nothing (`PORTING_PLAN.md` finding 29).
        minix_syscall: (nrRaw, a0, a1, a2, a3, a4, a5) => {
          const nr = Number(nrRaw);
          const dst = Number(a0);
          const msgAddr = Number(a1);

          if (st.pending !== null && st.pending.kind === 'yield') {
            // A replay after the host stopped this instance: the kernel was never asked, so fall
            // through and ask it now. `asyncify_stop_rewind` first, or the instrumented caller
            // re-enters its rewind path and traps.
            st.pending = null;
            st.inst.exports.asyncify_stop_rewind();
          } else if (st.pending !== null && st.pending.kind === 'resumed') {
            // A yield the host took back so it could dispatch whoever the yield made runnable.
            // The syscall itself *was* delivered — the kernel rotated the run queue and answered —
            // so there is nothing to ask it again: hand back what it returned and let the guest
            // carry on from the line after the yield.
            const value = st.pending.value;
            st.pending = null;
            st.inst.exports.asyncify_stop_rewind();
            return value;
          } else if (st.pending !== null) {
            // The kernel has answered, and the message is already in this instance's memory — put
            // there by the kernel's own delivery, through the copy seam. The host does not carry
            // it. Deliberately not the value cached at block time: a receive that blocked is
            // satisfied later, and `mini_send` stores the *sender's* endpoint in the receiver's
            // frame return slot, which has no other way to get there on this port.
            st.pending = null;
            st.inst.exports.asyncify_stop_rewind();
            return BigInt(kernel.exports.minix_proc_retval(spec.slot));
          }

          if (syscallsLeft <= 0) {
            exhaustedBy = spec.label;
            throw new BudgetExhausted(`${spec.label} spent the ${syscallBudget}-syscall budget`);
          }

          if (slice.used >= slice.limit) {
            // The slice is spent. Stop *before* entering the kernel, so nothing of the kernel's
            // is left half-done, and unwind the way a blocked syscall does: the guest serialises
            // itself and `run()` gets control back. This syscall is neither counted nor
            // delivered — the replay above does both.
            st.pending = { kind: 'yield', nr, msgAddr };
            st.inst.exports.asyncify_start_unwind(st.dataPtr);
            return 0n;
          }

          syscallsLeft -= 1;
          slice.used += 1;
          if (st.tail.length >= 8) st.tail.shift();
          st.tail.push({ nr, a0: dst });
          if (st.trace.length < 64) st.trace.push({ nr, a0: dst });
          st.syscalls += 1;

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

          // The observable shape of "the console has nothing for me": the tty's own `read(0)`
          // coming back `EAGAIN`, and the `thread_yield` its retry loop follows it with.
          if ((nr === NR_READ && dst === 0 && Number(result) === EAGAIN) || nr === NR_THREAD_YIELD) {
            slice.spin += 1;
          }

          if (kernel.exports.minix_proc_blocked(spec.slot) === 1) {
            st.pending = { kind: 'blocked', value: result, nr, msgAddr };
            st.inst.exports.asyncify_start_unwind(st.dataPtr);
            return 0n;
          }

          // A yield has to hand control back to the host, because switching processes is the
          // host's job: one instance runs at a time and `pump` is what picks the next. Inside a
          // dispatch nothing can pick, so a process that yields while another is runnable would
          // otherwise spend the rest of the slice spinning on the CPU that process is waiting for.
          // That is what a pointer drag over the desktop did — eight rounds of the input/desktop
          // handshake, one per slice, each slice filled with the tty's console-read retry
          // (`PORTING_PLAN.md` finding 62).
          //
          // Unconditional rather than gated on "is somebody else runnable": a yield means the
          // caller is done with the CPU, and asking the scheduler first would put the decision in
          // the host that the kernel has already made. `pump` ends the slice when a whole dispatch
          // did nothing but retry, so the round trip an idle guest pays is one, not the slice.
          if (nr === NR_THREAD_YIELD) {
            st.pending = { kind: 'resumed', value: result, nr, msgAddr };
            st.inst.exports.asyncify_start_unwind(st.dataPtr);
            return 0n;
          }
          return result;
        },
      },
    };
  }

  /// Enter the process in `st`: rewind it if it is resuming, else start its entry.
  function run(st) {
    if (st.pending !== null) st.inst.exports.asyncify_start_rewind(st.dataPtr);
    st.inst.exports[st.entry](...st.entryArgs);
    if (st.inst.exports.asyncify_get_state() === STATE_UNWINDING) return;
    // Returned with nothing to unwind: the entry is done. `minix-rt`'s `exit` traps rather than
    // returning, so this is a process that ran off the end of its entry point.
    st.exited = true;
    kernel.exports.minix_proc_exit(st.spec.slot);
  }

  // ---------------------------------------------------------------------- bring-up

  const serverModule = new WebAssembly.Module(serversBytes);
  const kernel = new WebAssembly.Instance(new WebAssembly.Module(kernelBytes), {
    env: {
      host_console_write: (b) => sink.write(b & 0xff),
      host_console_read: () => (queue.length === 0 ? -1 : queue.shift()),
      host_console_available: () => queue.length,
      host_cycles: () => {
        cycles += 1000;
        return BigInt(cycles);
      },
      host_halt: (code) => {
        haltCode = code;
      },
      host_copy_between: copyBetween,
      host_exec_module: hostExecModule,
      host_fork_process: hostForkProcess,
    },
  });

  kernel.exports.minix_kernel_init();

  // The endpoints are asked for rather than assumed: generation 0 makes an endpoint equal its
  // slot, but a hand-picked endpoint is what deadlocked M2, and `minix_make_endpoint` is the
  // answer to the same question the kernel will ask later.
  for (const s of specs) s.endpoint = kernel.exports.minix_make_endpoint(s.slot);

  for (const s of specs) {
    const st = makeServer(s);
    procs.push(st);
    bySlot.set(s.slot, st);
  }

  // The RAM disk is given the boot filesystem image. On the other arches the kernel maps it into
  // the server's address space at `RAMDISK_IMAGE_VA`; there is no kernel mapping here, so the host
  // writes it, and it has to land before the server's entry runs — that is when the device is
  // sized from the image's own superblock rather than from a constant.
  {
    const st = procs.find((p) => p.spec.label === 'ramdisk');
    if (st !== undefined) {
      const image = new Uint8Array(imageBytes);
      const needPages = Math.ceil((RAMDISK_IMAGE_VA + image.length) / WASM_PAGE);
      const havePages = st.memory.buffer.byteLength / WASM_PAGE;
      if (needPages > havePages) st.memory.grow(needPages - havePages);
      new Uint8Array(st.memory.buffer, RAMDISK_IMAGE_VA, image.length).set(image);
    }
  }

  const spawnFailures = [];
  for (const s of specs) {
    const st = procAt(s.slot);
    const r =
      s.spawnAsUser === true
        ? kernel.exports.minix_proc_spawn_user(s.slot, s.endpoint)
        : kernel.exports.minix_proc_spawn(s.slot, s.endpoint);
    if (r !== 0) spawnFailures.push(st);
  }

  // Start PM's chain. This is the kernel-state step `boot_init` performs on the shipping arches:
  // RS's notification is left pending on PM's privilege structure, and PM finds it on its next
  // RECEIVE. Deliberately not a message — nothing is sent between instances — which is why it is
  // a single bit to set rather than a copy.
  kernel.exports.minix_boot_notify();

  // -------------------------------------------------------------------------- pump

  /// Run guest work until it stops needing the host, or until the slice is spent.
  ///
  /// Returns why it stopped:
  ///
  ///   * `'quiescent'` — nothing is runnable.
  ///   * `'slice'` — `maxSyscalls` were used. The instance that hit the limit has been unwound and
  ///     is resumable; the front end decides whether to resume it now or to wait for input.
  ///   * `'budget'` — the syscall budget ran out: a guest is making syscalls without getting
  ///     anywhere, and the host has no way to help it.
  ///   * `'trapped'` — an instance faulted and the run cannot continue.
  function pump({ maxSyscalls = sliceSyscalls, maxSteps = 100000 } = {}) {
    slice = { used: 0, spin: 0, limit: maxSyscalls };
    let stepsThisPump = 0;
    for (;;) {
      const slot = kernel.exports.minix_step();
      if (slot === -1) return 'quiescent';
      if (++stepsThisPump > maxSteps) return 'steps';
      steps += 1;
      const st = procAt(slot);
      if (st === undefined) {
        note('the kernel picked a slot the host never spawned', `slot ${slot}`);
        return 'unknown-slot';
      }
      const usedBefore = slice.used;
      const spinBefore = slice.spin;
      try {
        run(st);
      } catch (e) {
        if (e instanceof BudgetExhausted) {
          note('the syscall budget ran out inside a single dispatch', e.message);
          return 'budget';
        }
        // A trap whose last syscall was EXIT is a process exiting, not failing. `minix-rt::exit`
        // issues `SYS_EXIT` and then traps, because this target gives the host no chance to run
        // while an instance is executing. The kernel has already done the bookkeeping —
        // SIGNALED | SIG_PENDING | SLOT_FREE, the queued status, and a notification to PM as
        // signal manager — so recording it is all that is left. It must *not* call
        // `minix_proc_exit`: that stores SLOT_FREE on its own, which would clear SIGNALED, and
        // SIGNALED is exactly what PM's GETKSIG loop looks for to find the exit.
        const last = st.tail.length > 0 ? st.tail[st.tail.length - 1] : null;
        if (last !== null && last.nr === NR_EXIT) {
          st.exited = true;
          continue;
        }
        note(
          `TRAP in ${st.spec.label} (slot ${st.spec.slot})`,
          `${e} — syscalls=${st.syscalls}, last=${st.tail.map((t) => `nr=${t.nr}`).join(',')}`
        );
        return 'trapped';
      }
      // A dispatch that did nothing but retry the console read, with nothing for the host to hand
      // either side: the guest is waiting for input, so end the slice here instead of spending the
      // rest of it spinning. The front end's park condition is the same one — this only reaches it
      // sooner, and with a yield handing control back (the `thread_yield` arm above) the whole idle
      // retry costs one round trip rather than a slice of them.
      if (
        slice.used > usedBefore &&
        slice.spin - spinBefore === slice.used - usedBefore &&
        console_.pending === 0 &&
        input_.pending === 0
      ) {
        return 'slice';
      }
      if (slice.used >= slice.limit) return 'slice';
    }
  }

  /// Whether the slice that just ended did nothing but retry the console read.
  ///
  /// This is the host's answer to "is the guest waiting for input?", and it is *observed* rather
  /// than inferred from a policy: every syscall in the slice was either the tty's `read(0)` coming
  /// back `EAGAIN` or the `thread_yield` that follows it. A front end may use it to park the
  /// system until a keystroke, which is what keeps an idle page off the CPU — see this file's
  /// header for why that is a workaround for finding 32 rather than a design.
  const sliceWasSpinOnly = () => slice.used > 0 && slice.spin === slice.used;

  return {
    kernel,
    procs,
    procAt,
    memoryFor,
    copyLog,
    forks,
    /// What the host's block device has been asked for, and whether one is attached at all.
    device: deviceStats,
    /// What the display has been asked for: frames presented, bytes, and the mode it named.
    display: displayStats,
    console: console_,
    /// The input queue, and the two ways to put a record in it: `push` announces it (what a front end
    /// does), `enqueue` holds it silently (what a check does to prove the announcement is what the
    /// guest acts on).
    input: input_,
    spawnFailures,
    pump,
    sliceWasSpinOnly,
    /// How much of the budget is left, and which instance spent it if it is gone.
    get budget() {
      return { left: syscallsLeft, exhaustedBy };
    },
    /// Set when the guest asks the host to stop (`host_halt`).
    get halted() {
      return haltCode;
    },
    get steps() {
      return steps;
    },
    /// How many copies the kernel has asked for, and how many bytes they moved. The count is of
    /// requests, the bytes of successes.
    get copies() {
      return { count: copies, bytes: bytesCopied };
    },
    /// The last slice's shape, for a status line.
    get slice() {
      return { ...slice };
    },
  };
}
