---
name: minix-c-to-rust
description: Guidelines for translating MINIX 3.3.0 C code to Rust — struct layout, pointer patterns, type conversion, endianness. Also covers the project's "no stubs" policy for todo!() usage and stub deferral rules. Load automatically when porting from or debugging against `.refs/minix-3.3.0/`.
---

## No Stubs — Real Implementations Only

**Write real code. Do not stub out functionality with `unimplemented!()`, `panic!("not yet")`, or empty `todo!()` calls.**

Every function you write must do something meaningful. If a feature needs infrastructure that does not exist:
1. **Implement the missing infrastructure first** — it becomes the prerequisite task.
2. **If you cannot implement it in this session**, add a new task to PORTING_PLAN.md describing the
   missing functionality, then use `todo!("<explanation; see PORTING_PLAN.md TASK>")`.

### Good `todo!()`
```rust
todo!("Read config from user's shell preference; see NEXT.md T3.1");
```

### Bad `todo!()`
```rust
todo!();                          // no explanation
todo!("implement later");        // vague
fn f() {}                         // empty body, silent no-op
```

**Stub Deferral Rule:** Every `todo!()` or `stub_handler!` MUST create a concrete task in
PORTING_PLAN.md under the Phase where the missing dependencies become available.
Do NOT leave "deferred to later phase" without a traceable task.

# C to Rust Translation for MINIX

## `#[repr(C)]` Struct Layout

**Always use `#[repr(C)]`** for structs shared between C and Rust or stored on disk.

- Fields in the same order as C
- Same types (same sizes + signedness)
- For packed structs (no padding): `#[repr(C, packed)]`
- Verify with compile-time assertions: `core::mem::offset_of!(T, field)` tests in unit tests

### D2Inode (on-disk inode) — Correct Translation

```c
// C (minix/fs/mfs/type.h)
typedef struct {
  u16_t d2_mode;
  u16_t d2_nlinks;
  i16_t d2_uid;
  u16_t d2_gid;
  i32_t d2_size;
  i32_t d2_atime;
  i32_t d2_mtime;
  i32_t d2_ctime;
  zone_t d2_zone[V2_NR_TZONES];  // u32[10]
} d2_inode;
```

```rust
// Rust (crates/fs/src/mfs/types.rs)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D2Inode {
    pub d2_mode: u16,
    pub d2_nlinks: u16,
    pub d2_uid: i16,
    pub d2_gid: u16,
    pub d2_size: i32,
    pub d2_atime: i32,
    pub d2_mtime: i32,
    pub d2_ctime: i32,
    pub d2_zone: [u32; V2_NR_TZONES],  // 10
}
```

### Type Mapping

| C Type | Rust Type | Size |
|--------|-----------|------|
| `char`, `u8_t` | `u8` | 1 |
| `u16_t` | `u16` | 2 |
| `i16_t` | `i16` | 2 |
| `u32_t` | `u32` | 4 |
| `i32_t` | `i32` | 4 |
| `u64_t` | `u64` | 8 |
| `i64_t` | `i64` | 8 |
| `zone_t` | `u32` | 4 |
| `block_t` | `u32` | 4 (32-bit) / `u64` (64-bit) |
| `off_t` | `i32` | 4 (MINIX V3) |
| `endpoint_t` | `i32` | 4 |
| `mode_t` | `u16` | 2 |

### Message Struct (ABI-Critical)

Verified at compile time:
```rust
#[test]
fn test_message_offsets() {
    assert_eq!(offset_of!(Message, m_source), 0);
    assert_eq!(offset_of!(Message, m_type), 4);
    assert_eq!(offset_of!(Message, m_payload), 8);
}
```

Total size = 56 bytes (`m_source(4) + m_type(4) + m_payload(48)`).

## C Macro → Rust Translation

### `conv2` / `conv4` — Byte Swapping

```c
#define conv2(norm, w) (norm ? (unsigned)(w) : swap16(w))
#define conv4(norm, x) (norm ? (unsigned long)(x) : swap32(x))
```

```rust
pub fn conv2(norm: i32, w: i32) -> u32 {
    if norm != 0 { (w as u32) & 0xFFFF }
    else { /* byte swap 16-bit */ }
}
pub fn conv4(norm: i32, x: i64) -> i64 {
    if norm != 0 { x }
    else { /* byte swap 32-bit */ }
}
```

`norm = 1` means native byte order (no swap). On little-endian x86_64 with a little-endian MINIX FS image, `norm` is always `1`.

### `b_v2_ino` — Pointer Arithmetic

```c
#define b_v2_ino(b) ((d2_inode *) (b)->b_addr)
```

In Rust, compute the offset directly:
```rust
let dip2_offset = ((i_num - 1) % inodes_per_block) * size_of::<D2Inode>();
let dip2 = (*bp).data_ptr.add(dip2_offset) as *mut D2Inode;
```

## Common Pointer Patterns

### `&mut *sp` on a raw pointer (giving a reference)

```c
sp = get_super(dev);
```

```rust
let sp = super_block::get_super(dev);
if sp.is_null() { return EINVAL; }
// sp is *mut SuperBlock — use unsafe dereference
```

### Global state access

```c
// C — globals are just variables
extern struct mfsglobal mfs;
mfs.err_code = 0;
```

```rust
// Rust — raw pointer to static
let mfs = glo::mfs_ptr();
(*mfs).err_code = 0;
```

## Error Handling

| C Idiom | Rust Idiom |
|---------|-----------|
| `return EINVAL;` | `return EINVAL;` (same — error codes are negative `i32`) |
| `if (r != OK) return r;` | `if r != OK { return r; }` |
| `*p = value;` | `(*ptr).field = value;` |
| Null pointer check | `if ptr.is_null() { ... }` |

MINIX error codes are negative integers (0 = OK, -1 = EPERM, -2 = ENOENT, etc.). Use `i32` and compare with named constants from `arch_common::ipc`.

## On-Disk Inode Numbering

**Critical distinction:** MINIX inode numbers are **1-based**. Inode 1 is the root inode. The on-disk inode table stores inode N at **slot (N-1)**. When reading:
- Block number: `(i_num - 1) / inodes_per_block + inode_table_start_block`
- Slot within block: `(i_num - 1) % inodes_per_block`

The FS image builder (`tools/mkminixfs.rs`) must follow the same convention — **no dummy inode 0** at slot 0.

## Reference Source Files

All original C source is in `.refs/minix-3.3.0/`. Key paths:

| Component | C Source |
|-----------|----------|
| IPC kernel | `.refs/minix-3.3.0/minix/kernel/proc.c` |
| VFS init | `.refs/minix-3.3.0/minix/servers/vfs/main.c` |
| PM init | `.refs/minix-3.3.0/minix/servers/pm/main.c` |
| MFS inode | `.refs/minix-3.3.0/minix/fs/mfs/inode.c` |
| MFS superblock | `.refs/minix-3.3.0/minix/fs/mfs/super.c` |
| Message types | `.refs/minix-3.3.0/minix/ipc.h` |
