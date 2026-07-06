---
name: minix-server-patterns
description: MINIX server architecture patterns — main loop, SEF callbacks, dispatch tables, global state. Use when implementing or debugging PM, VFS, MFS, RS, DS, or any server process.
---

# MINIX Server Patterns

## Server Structure

Every MINIX server follows the same pattern:

```
main()
  ├─ init()           — initialize globals, register callbacks, set up subsystems
  └─ main_loop()      — receive → dispatch → reply (forever)
       ├─ receive(ANY, &msg)
       ├─ dispatch(req_nr)
       └─ sendrec(who, &reply)
```

## Concrete Example: MFS Main Loop

File: `crates/fs/src/mfs/main.rs`

```rust
pub fn mfs_main() -> i32 {
    mfs_init();  // init globals, buffer cache, RAM disk I/O
    
    loop {
        // Receive message from any process
        let src = syscall2(RECEIVE_CALL, ANY, &msg);
        
        // Extract request number and caller credentials
        let req_nr = msg.m_type - FS_BASE;
        let (caller_uid, caller_gid) = unsafe { msg.m_payload.m1.m1i1 as u16, ... };
        
        // Store in globals for handler access
        glob.m_in = msg;
        glob.req_nr = req_nr;
        glob.caller_uid = caller_uid;
        glob.caller_gid = caller_gid;
        
        // Dispatch
        let status = dispatch(req_nr);
        
        // Reply
        let reply = build_reply(glob.m_out, status);
        syscall2(SENDREC_CALL, src, &reply);
    }
}
```

## Concrete Example: VFS Main Loop

File: `crates/servers/src/vfs/main.rs`

VFS uses a `get_work() / handle_work()` pattern with worker threads:

```rust
pub fn vfs_main() -> i32 {
    sef_local_startup();  // SEF init (syscall-based)
    
    loop {
        // get_work() receives a message and assigns it to a worker thread
        get_work();
        
        // handle_work() dispatches based on message type
        handle_work();
    }
}
```

VFS calls `sef_cb_init_fresh()` during `sef_local_startup()`, which (in the simplified Rust version) initializes the fproc table, dmap table, grant table, and calls `mount_root()`.

## SEF (System Event Framework)

SEF is a MINIX convention for server initialization and live update support. In Rust, it's simplified:

- `sef_local_startup()` — calls the appropriate init callback based on how the server was started (fresh start, restart, live update)
- `sef_cb_init_fresh()` — called on first boot. In the Rust VFS, this is where fproc init, dmap init, grant init, and mount_root happen
- The full C SEF protocol (PM→VFS_PM_INIT handshake, DS subscription, map_service from RS) is **not implemented** in Rust. See BLOCKERS.md.

## Global State Pattern

MINIX servers use global mutable state accessed through raw pointers (to avoid `static mut ref` in Rust 2024):

```rust
#[repr(C)]
pub struct MfsGlobal {
    pub m_in: Message,
    pub m_out: Message,
    pub req_nr: i32,
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub fs_dev: u32,
    // ... more fields
}

// Stored in static with UnsafeCell<MaybeUninit<T>>
static MFS_STORAGE: MfsStorageCell = MfsStorageCell::new(MaybeUninit::uninit());

pub unsafe fn mfs_ptr() -> *mut MfsGlobal {
    MFS_STORAGE.get().cast()
}
```

**Never** create references to `static mut` — use `addr_of_mut!` and raw pointer dereference.

## Dispatch Table Pattern

FS (MFS) dispatch in `crates/fs/src/mfs/table.rs`:

```rust
pub fn dispatch(req_nr: usize) -> i32 {
    // req_nr = msg.m_type - FS_BASE
    match req_nr {
        1  => fs_readsuper(),
        2  => fs_putnode(),
        3  => fs_read(),
        4  => fs_write(),
        // ...
        _  => ENOSYS,
    }
}
```

VFS dispatch in `crates/servers/src/vfs/call.rs` uses a similar match on message type constants.

## RAM Disk Block I/O

MFS reads the MINIX FS image from a memory region mapped into its address space at boot:

```
MFS_RAMDISK_VA  — virtual address of the RAM disk in MFS's address space
MFS_RAMDISK_SIZE — size of the RAM disk
```

Initialization:
```rust
crate::block_io::ram_disk_init(MFS_RAMDISK_VA as *const u8, MFS_RAMDISK_SIZE);
libs::libminixfs::cache::lmfs_set_block_io(crate::block_io::ram_disk_io);
```

The `ram_disk_io` function reads/writes blocks by `memcpy` from/to the RAM disk memory region. No actual disk I/O is involved.

## Buffer Cache

MFS uses a block buffer cache (`libs::libminixfs::cache`) shared by all MINIX FS-family servers:

- `lmfs_get_block(dev, block_nr)` — get a buffer for reading
- `lmfs_put_block(bp, block_type)` — release buffer
- `lmfs_markdirty(bp)` — mark buffer as dirty (needs write-back)
- `lmfs_set_blocksize(size, major)` — set block size (4096 for MINIX V3)
- `lmfs_buf_pool(nr_bufs)` — allocate buffer pool
