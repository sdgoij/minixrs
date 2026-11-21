# xtask_migration.md: one Rust entry point for the build, test and gate scripts

## Verdict

Adopt an `xtask`, but **do not replace `just`**. Keep `just` as the front door
(215 recipes, all of them named in CI, `AGENTS.md`, `.notes/` and 22 skill
documents) and move the *logic* behind the recipes into a host-only
`crates/xtask` crate, one cluster at a time. The first slice that pays for
itself is `check-static`, three gate scripts that run on every push and have no
tests today; the QEMU scenario driver is the riskiest and comes after the build
and image layers have built the process helpers it needs.

Concretely: Option B in section 4, phases 0 to 5 in section 6, and the
non-goals in section 7.

## 1 What `xtask` is, and what it is not

`xtask` is two separate things, and conflating them is the usual way these
proposals go wrong.

**The pattern.** A binary crate in the same cargo workspace, usually
`crates/xtask`, invoked through a cargo alias:

```toml
# .cargo/config.toml
[alias]
xtask = "run --quiet --package xtask --target-dir .xtask --"
```

The value is that a build task becomes an ordinary Rust program: the same
compiler, the same lockfile, the same `cargo test` and `cargo clippy
--all-targets` that the repo already runs. There is no framework, no
`[[bin]]` search path magic, and no second package manager. This is what the
rust-analyzer, Cargo and Firefox trees do, and it is what the project rule
"prefer implementing in an existing file unless the change is a new logical
component" points at: an `xtask` is a logical component.

**The crate, `xtask` 0.1.4 (docs.rs, MIT/Apache-2.0, published 2026-11-14).**
Its own documentation is explicit that it is *not* a framework: "Each
repository should keep its own `clap` command tree and product-specific build
flow." It supplies the primitives that get copy-pasted between repositories.
Against our needs:

| Module | Feature | Use to us |
| --- | --- | --- |
| `repo::root_from_xtask_manifest` | default | Yes, small and exactly right |
| `fs::ensure_dir` and friends | default | Marginal, `std::fs` is two lines |
| `process` (an `xshell` setup layer) | `process`, default | Maybe, if we want `xshell` anyway |
| `hash::sha256_file`, artifact sizes | `hash`, default | Yes, `fetch-stage1` needs sha256 |
| `github_actions` (`GITHUB_OUTPUT`) | opt-in | Maybe, for the release pin scripts |
| `latest_json`, `sparkle`, `apple`, `dmg` | opt-in, macOS | No |
| `arch` (release arch names) | default | No, our arch names are our own |

So the crate is worth a dependency only if we are also taking `xshell`. If the
goal is "few new dependencies" (`[workspace.dependencies]` lists exactly one,
`bitflags`), the four things we actually want from it are about 120 lines of
`std`. My recommendation is to defer the decision until the phase that first
needs `hash` or `process` (phase 2), then depend on it with
`default-features = false, features = ["process", "hash"]`. Phase 0's checkers
spawn nothing and hash nothing, so they need neither.

What `xtask` will not do, and this matters more than what it will: it will not
give us command discovery (`just --list`), recipe dependencies, or parse-time
variables. Those are `just` features, and section 4 explains why we keep them.

## 2 What exists today

### 2.1 Inventory

| Cluster | Files | Lines | Invoked from | Candidate |
| --- | --- | --- | --- | --- |
| Python, top level (`tools/*.py`) | 95 | 14,609 | `Justfile`, CI, other `.py` | partly |
| Rust, top level (`tools/*.rs`) | 8 | 1,850 | `Justfile` via stage1 `rustc` | yes |
| POSIX sh (`tools/smoke/feed.sh`, `wasm-verify.sh`) | 2 | 352 | `Justfile`, CI | one of them |
| C / asm (`tools/*.c`, `tools/*.S`) | 24 | 3,057 | build scripts, boot | no |
| C headers (`tools/c-include/`) | contract | | C and C++ consumers | no |
| Scenarios (`tools/smoke/*.tsv`) | 46 | data | four targets | no |
| wasm harness (`tools/wasm-*`, Node) | | | CI, `publish-wasm` | no |
| Archaeology (`tools/probes/`) | 406 | | nobody | no |

`Justfile` is 2,052 lines and 215 recipes: 128 `test-*`, 13 `build-*`, 4 each
of `run`, `mkfs`, `image`, `gltriangle`, `export`, `desktop`.

### 2.2 The Python, clustered by what it actually is

1. **Toolchain discovery and pinning.** `stage1.py` (67), `lld.py` (174),
   `pin.py` (38), `bash_pin.py`, `mesa_pin.py`,
   `fetch-mesa.py` (186),
   `fetch-bash.py` (179), `mirror-bash.py`, `ccarch.py`, `ccflags.py`.
   Mostly path resolution, small files, a few network fetches. The pin modules
   are *imported as modules* by release workflows (section 3.6).
2. **Build wrappers around clang, rustc and meson.** `build-c-hello.py` (170,
   now `cargo xtask build c-hello`), `build-std-hello.py` (153, now
   `cargo xtask build std-hello`), `build-dynlibc.py` (147), `build-dynlink.py`
   (255), `build-gltest.py`, `build-gui.py`, `build-gbmtest.py`,
   `build-wlcompose.py`, `build-wlgl.py`, `build-wldmabufgl.py`,
   `build-bash.py` (422), `build-mesa.py` (478), `build-libcxx.py` (153),
   `libcxx-toolchain.py`, `cc-minix.py` (194), `cc-dso-minix.py` (407),
   `cdyn.py` (162), `lld.py`'s flag helpers.
3. **Image and boot assembly, already Rust.** `mkboot.rs` (241, live),
   `mkfs.rs` (287, live), `mkinitramfs.rs` (376) and `mkminixfs.rs` (561),
   both superseded by `crates/boot-image` (see section 3.4), `mkimg.rs` (287,
   explicitly unwired), `llvm-config-minix.rs` (194, no caller).
4. **Gates.** `check-c-headers.py` (147), `check-physmap.py` (100),
   `check-justfile.py` (97), `gen-c-headers.py` (205).
5. **The QEMU harness.** `smoke/feed.sh` (277 lines of hard-won POSIX sh),
   `smoke/interactive_bash.py` (104), `piped_matrix.py` (164), and the CI
   probes: `fbmmap_probe.py` (179), `wlfull_probe.py` (302),
   `dso_share_probe.py` (570), `wldmabufgl_probe.py` (199), `wlscene_probe.py`
   (200), `wlcompose_probe.py`, the `wl*_probe.py` family.
6. **One-off diagnostics.** Roughly 30 top-level `*_probe.py`, `fb_*.py`,
   `su_*.py`, `tcp_*.py`, `udp_*.py`, `persist_*.py`, `rc_*.py`, plus 406
   files under `tools/probes/`.

### 2.3 Why the shell layer costs us, in the project's own measurements

These are not speculative. Each is a comment in `Justfile`, `feed.sh` or a trap
in `AGENTS.md`, and each is a failure mode that a Rust process layer removes by
construction:

- **`timeout` is not `timeout` on Windows.** A recipe shell inherits System32
  ahead of the MSYS bin dir, so `timeout` resolves to `TIMEOUT.EXE` with an
  unrelated command line. `Justfile` spells `/usr/bin/timeout` out, and the
  same trap applies to `find`, `sort` and `more` (Justfile L381-384).
- **MSYS rewrites POSIX-looking values on the way to a native tool.**
  `MINIXFS_EXTRA=/bin/bash=...` becomes `/bash`, the file lands at the root
  filesystem, and every listing of `/bin` says it is missing. The fix is a
  process-wide `MSYS2_ENV_CONV_EXCL` export, applied to every recipe so nested
  `just` invocations inherit it (Justfile L42-56). A native `xtask` spawning
  the native assembler has no conversion to suppress.
- **A FIFO that a native QEMU reads nothing from.** Measured, and the reason
  `feed.sh` feeds on the left side of a pipe and reports through a status file
  instead of an exit status (feed.sh L10-17).
- **Input before the prompt is dropped, not buffered.** Measured on the
  shipped image, and the reason the driver paces and waits per step
  (feed.sh L18-22).
- **The driver's verdict must outlive the guest.** `feed.sh` is bounded by a
  budget, a backstop and a status file because "a gate that never spoke has
  failed" (AGENTS.md).
- **`python` must exist by that name.** Every CI job that builds anything adds
  `sudo ln -sf /usr/bin/python3 /usr/local/bin/python` (ci.yml L165, L247-248,
  L318-319, L539-540, L686-687, L819-820). Six jobs, one shim, one more
  thing a fresh Linux image can get wrong.
- **Every `just` invocation runs Python twice.** `stage1-rustc := \`python
  tools/stage1.py\`` and `minix-lld := \`python tools/lld.py\`` are parse-time
  variables (Justfile L25, L32), so `just --list` spawns two interpreters.

That is a fair amount of accidental complexity sitting directly on the critical
path, and it is the honest argument for this migration. The argument is *not*
"Python is bad": the scripts are well documented and careful. It is that the
build and gate layer is the one place where the host platform leaks into the
product, and Rust is the only language already guaranteed to be present.

## 3 Constraints that decide the design

### 3.1 `cargo clean` cannot delete a running `xtask.exe`

`just bootstrap` and `_finish-bootstrap` run `cargo clean` (Justfile L103,
L106) to drop rlibs built by the previous toolchain, because a stale cache
surfaces later as `E0463` or a clap `TypeId` panic (AGENTS.md, "Traps that keep
catching us", and the comment at L90-101).

If `bootstrap` becomes `cargo xtask bootstrap`, the running binary lives in
`target/debug/xtask.exe`, and on Windows `cargo clean` cannot delete a mapped
executable. This is not a theory: it is an `os error 32` and a failed
bootstrap.

The fix is in the alias, before anything is written:

```toml
xtask = "run --quiet --package xtask --target-dir .xtask --"
```

`cargo clean` with no `--target-dir` removes `./target`, so `.xtask/` survives.
The alternatives (re-exec from a temp copy, or a deferred clean with a marker
file) are worse: the first needs `--root` plumbing, the second turns a clean
tree into a state machine. `.gitignore` gains one line.

### 3.2 The tree must be parseable before the toolchain exists

`just bootstrap` on a fresh machine has to work, so any parse-time variable it
needs must resolve without a built toolchain. `stage1.py` and `lld.py` print
nothing in that case, by design ("`just bootstrap` on a host that has not built
one yet still has to be able to parse the Justfile", stage1.py L25-27).

Consequence: this is the one place where an `xtask` subcommand cannot replace
a script outright. Pointing the backtick at `cargo xtask which rustc` would make
every `just` invocation, including `just --list`, build `xtask` first, and on a
fresh clone that is a build ahead of the toolchain it is trying to locate. The
same applies to the ambient export every recipe inherits:

```make
export CARGO_TARGET_X86_64_PC_MINIX_LINKER := minix-lld
```

The plan therefore keeps `tools/stage1.py` and `tools/lld.py` (241 lines
between them, pure path logic) as the only permanent Python in the tree, and
migrates their *consumers* instead of the lookup. Open question 3 reopens this
if we decide otherwise.

### 3.3 `mkboot.rs` is compiled by the stage1 `rustc` today

`Justfile` L212 runs `"{{stage1-rustc}}" tools/mkboot.rs --edition 2024 -o
target/mkboot`, and runs it from a recipe that already required a bootstrap.
Nothing in `mkboot.rs` needs the fork: it is a `std` program with a
`#[path = "../crates/kernel-boot/toolchain_tools.rs"]` include (that file is now
the `host-tools` crate, which the build script and the xtask both depend on).

Moving it into `xtask` (built by the pinned host toolchain, `1.96.0` per
`rust-toolchain.toml`) removes a stage1 dependency from the image path and
removes the compile-and-run two-step. It does add a requirement: the host
`cargo` must exist before `build-<arch>`. That is already true for every
recipe that runs `cargo build`, and true of the machine that can run `just` at
all, but it is the one thing to verify in phase 2 rather than assume.

### 3.4 Half the Rust in `tools/` is already dead

`tools/mkinitramfs.rs` and `tools/mkminixfs.rs` were ported into
`crates/boot-image` (`cpio.rs` and `minixfs.rs` say so in their doc comments),
`tools/mkimg.rs` is called "unwired" by Justfile L365 and
`image-release.yml` L15, and `tools/llvm-config-minix.rs` has no caller. Only
`mkboot.rs` and `mkfs.rs` are live.

So this part of the migration is not a rewrite: it is two files moving into a
crate, plus three files deleted with a note in the commit message. It should be
the cheapest phase in the plan, not the most expensive.

### 3.5 The gates are contracts, and their output is the interface

`check-static` is three checkers that print "one line per offender" and exit 1
(`check-physmap.py` L18, `check-justfile.py` L18). A port must keep the line
format, the exit status and the success line, because `just check` and CI's
`host-tests` job both just check the status today, but a human reads the lines.

`check-justfile.py` is also load-bearing in a subtle way: it exists because
`crates/kernel/build.rs` reads *every* `BOOT_BINS` entry and panics on a
missing one, and a warm `target/` hides a missing recipe dependency. A port
should get *stronger*, not merely equivalent: parse `BOOT_BINS` out of
`crates/boot-image/src/manifest.rs` instead of hardcoding the three producer
names, so the checker fails when a new boot binary is added without a producer.

### 3.6 The pin modules are imported, not executed

`bash-release.yml` L123-124 and `mesa-release.yml` L174-177 do:

```sh
python3 -c 'import sys;sys.path.insert(0,"tools");import mesa_pin as p;print(p.MESA_TAG)'
```

and `ci.yml` L603, L740 call `python3 tools/bash_pin.py` /
`python3 tools/mesa_pin.py` for a tag. Any migration of the pins must either
keep that interface (a Python shim) or change those five workflow lines in the
same commit. The second is fine, but it is not a free move and should be its
own change.

### 3.7 `xtask` is host-only and must stay out of the minix graph

The workspace excludes `kernel-sim-tests` and `kernel-tests` precisely so cargo
feature unification cannot reach the real boot binaries (Cargo.toml L4-25).
`crates/xtask` is a `std` binary for the host. It must not grow a dependency on
a target crate that could unify features into a minix build. Where a constant
is needed (the `BOOT_BINS` names), reading the source text is both safe and
what the current checker already does.

### 3.8 Host clippy and host tests will see it

`cargo clippy --all-targets -- -D warnings` and `cargo test --workspace` run in
CI and in `just check`. A new member is new code that must be warning-free and
tested. That is a benefit (the scripts have no tests today) and a cost (phase 0
must land clean, not "mostly clean").

### 3.9 The `.tsv` scenarios are read by four targets

`AGENTS.md` is explicit: `tools/smoke/scenario.tsv` is one scenario read by the
image boot on all three hardware arches and by `run.js` on wasm. If the QEMU
driver moves to Rust, the data contract does not move, and neither does the
step semantics (tab separator, whole-echo acceptance, per-step byte anchor,
prompt-before-send). That is why the harness phase is last and has its own
parity suite.

### 3.10 The scripts share a helper layer, so deletions lag migrations

`ccarch.py`, `ccflags.py`, `lld.py`, `stage1.py` and `pin.py` are imported as
modules by roughly twenty scripts (`build-gltest.py` imports three of them). A
deletion therefore cannot follow the phase that first stops using it: it follows
the phase that removes its **last importer**. `ccflags.py` has none left after
the build-wrapper phase, `ccarch.py` keeps two (`build-bash.py`,
`build-mesa.py`) until the third-party phase, and `lld.py` keeps two
(`stage1.py`, `rc-cost.py`) for good.

The phase table says where each file dies. The plan is not "every phase deletes
what it ported"; it is "every phase deletes what nothing imports any more", and
a phase that leaves a shared module behind says so.

## 4 Options

| | A. Replace `just` | B. Shim the recipes | C. Extract only the library | D. Python package |
| --- | --- | --- | --- | --- |
| Entry point | `xtask <cmd>` | `just <recipe>` unchanged, body is `cargo xtask ...` | mixed | `python -m minixrs` |
| CI changes | 5 workflows, all `just` calls | none in phase 0 | none | none |
| Docs changes | 22 docs + 23 with paths + 215 recipes | one line per migrated recipe | none | none |
| Discovery (`just --list`) | lost | kept | kept | lost |
| Windows/MSYS traps | removed | removed per cluster | partly | unchanged |
| Testable in `cargo test` | yes | yes | yes | only with pytest |
| Risk | high, one big bang | low, per cluster | low | low |
| Removes Python from the loop | eventually | eventually, except the two lookups (question 3) | no | no |

**A is rejected**, not because it is wrong in principle but because the diff is
the whole repository's interface at once, and the failure mode is "the thing
that boots is broken". 215 recipes, five workflows and every example in
`AGENTS.md` and the skills would have to move in one commit that could not be
bisected.

**D is worth stating and rejecting on the record.** A `tools/minixrs/` package
with `argparse` would get testability and one CLI without touching the runner,
and if the objection to the current state were "discoverability", it would be
the right answer. It does not remove `python` from CI, does not remove the MSYS
traps (those are in the shell and in QEMU spawning, not in Python), and adds a
second convention alongside the scripts that stay. It is a fallback if we
decide mid-migration that Rust is the wrong tool for one cluster.

**C is a subset of B** and is a perfectly good stopping point: it is what you
get if phases 0 to 3 land and phases 4 and 5 are judged not worth it.

**B is the recommendation.** One recipe body changes per migrated command, e.g.

```make
# today
check-static:
    python tools/check-c-headers.py
    python tools/check-physmap.py
    python tools/check-justfile.py

# after phase 0
check-static:
    cargo xtask check static
```

The recipe name, its arguments, its exit status and its place in `just check`
all stay. The acceptance criterion for a phase is that the recipe's
`just`-level behaviour is unchanged; the implementation is new.

## 5 The crate

```
crates/xtask/
  Cargo.toml            # name = "xtask", [[bin]] xtask, host only
  src/main.rs           # clap tree, dispatch, one verdict line per command
  src/root.rs           # repo root + path lookups (target/, tools/, rust/build)
  src/toolchain.rs      # arch table and cc flags (from ccarch.py, ccflags.py);
                        # the stage1/lld lookup stays in tools/ (section 3.2)
  src/check/
    mod.rs              # the three static gates, one module each
    c_headers.rs        # from check-c-headers.py
    physmap.rs          # from check-physmap.py
    justfile.rs         # from check-justfile.py, plus BOOT_BINS from manifest.rs
  src/cc.rs             # clang/rustc/lld invocation shared by the build wrappers
  src/image.rs          # mkboot, mkfs post-link steps (from tools/*.rs)
  src/smoke/            # phase 4: scenario parsing, guest driving, log asserts
  src/qmp.rs            # phase 5: one QMP client for the probes that are gates
```

Dependencies, and the reason for each:

| Crate | Why |
| --- | --- |
| `clap` (derive) | 20 or so subcommands with flags; hand-rolled parsing is not better |
| `anyhow` or `thiserror` | error context that names the missing tool, which is the project's stated policy |
| `xshell` (optional) | only if we want line-by-line command echo, which the scripts have today |
| `sha2` (optional) | `fetch-stage1`; `xtask`'s `hash` feature already wraps this |
| `tar` + `xz2` (optional) | `fetch-stage1` extracts `stage1-<sha>-<host>.tar.xz` |

Conventions, borrowed from the scripts they replace:

- **One verdict line per command**, on stdout or stderr, ending with what
  passed. `AGENTS.md`: "a gate that never spoke has failed".
- **A timeout is enforced by the harness**, not by an external `timeout`. The
  process layer owns the deadline, kills the child, and prints the verdict. No
  `/usr/bin/timeout`, no TIMEOUT.EXE.
- **`--dry-run` prints the exact command lines** and mutates nothing. The
  scripts print their `running: ...` lines already; this makes them the
  contract.
- **Missing tool = named failure.** Never a silent pass, never a machine-local
  path. This is already the shape of `mkboot.rs`'s `tool()` helper and
  `lld.py`'s docstring.
- **Exit 1 with one line per offender** for the checkers, unchanged.

## 6 Phased plan

Each phase is independently mergeable, leaves every gate green, and ends by
deleting what nothing imports any more (section 3.10), not necessarily what it
ported. A phase that leaves a shared Python module behind records why.

| Phase | Scope | New lines (est.) | Risk | Gate that must pass | Delete |
| --- | --- | --- | --- | --- | --- |
| 0 | crate, alias, `--list`, `check static` | ~700 | low | `just check`, `cargo clippy --all-targets`, `cargo test --workspace` | 3 checkers |
| 1 | the C/Rust build wrappers: `cc-minix`, `cc-dso-minix`, `build-c-hello`, `build-std-hello`, `build-libcxx`, `libcxx-toolchain`, `cdyn`, `build-dynlibc`, `build-dynlink`, the `build-*` probe clients, plus `ccarch`/`ccflags` as Rust | ~2,600 | medium | `just bootstrap`, `just build-c-hello <arch>`, `just libcxx <arch>`, `just gltriangle <arch>`, `just dynlib-<arch>` | ~17 scripts, and `ccflags.py` |
| 2 | image and fetch: `boot mkboot`, `mkfs`, `fetch stage1`, `verify-stage1`, `rust-config` | ~800 | medium | `just build-x86`, `just mkfs-<arch>`, `just image x86`, `just fetch-stage1`, `just verify-stage1` | `mkboot.rs`, `mkfs.rs`, 3 superseded `.rs`, 3 scripts |
| 3 | third-party builds: `build bash`, `fetch bash`, `mirror bash`, `build mesa`, `fetch mesa`, the pins | ~1,400 | medium | `just build-bash x86`, `just libcxx x86`, `just gltriangle x86`, `just fetch-mesa x86` | 7 scripts, then `pin.py` and `ccarch.py` |
| 4 | QEMU harness: `smoke`, `assert-log`, `interactive-bash`, timeouts | ~1,000 | high | `just image <arch>`, `just test-bash-<arch>`, `just test-interactive-bash-<arch>` | `feed.sh`, `interactive_bash.py` |
| 5 | CI gate probes: `probe read-hist`, `rc`, `wlkey`, `wlfocus`, `wlpointer`, `wlmotion`, `wldemo`, `wlpanel`, `wlfull`, `wldmabuf`, `wldmabufgl`, `wlcompose`, `wlscene`, `dso-share` | ~1,900 | medium | `just test-wlfull <arch>`, the wl input gates, `just probe-dso-share-x86` | 14 probes |

After phase 5 the Justfile still exists and still has its 215 recipes, and no
recipe body runs a Python script except the two parse-time lookups in
`stage1.py` and `lld.py`.

**Phase 0 landed.** `crates/xtask` exists with the three gates as
`cargo xtask check static|headers|physmap|justfile`, the alias is in
`.cargo/config.toml` with its own `--target-dir .xtask`, `check-static` is one
line, and `tools/check-c-headers.py`, `tools/check-physmap.py` and
`tools/check-justfile.py` are gone with every reference updated. Each rule is a
function over text with fixture tests; the port was checked against the Python
on the real tree, on a passing tree and on a failing one, before the scripts
were deleted.

**Phase 1, two slices landed.** `cargo xtask build c-hello|std-hello` replaces
the two builders `bootstrap` uses, with the arch table (`arch.rs`), the hermetic
C flags (`cc.rs`) and the toolchain lookup (`toolchain.rs`) as Rust. Then
`cargo xtask build dynlibc|dynlink|cdyn` replaced the dynamic-linking trio, on
the shared link core in `dynamic.rs` (`dyn` is a Rust keyword). Then
`cargo xtask build libcxx|libcxx-toolchain` replaced the C++ runtime build: the
merged `libstdc++.a` was byte-identical to the Python build's, and the generated
cross file differs only in its header comment (which named the deleted script).
The four smoke binaries, the twelve dynamic artifacts and the archive were
byte-identical to the Python build's before the scripts went, and
`crates/kernel/build.rs`'s "build it with ..." errors name the new commands.
Still Python: `cc-minix`, `cc-dso-minix`,
`build-bash`/`fetch-bash`/`build-mesa`, and the `build-*` probe and Wayland
clients. `ccarch.py` and `ccflags.py` stay with them; `crates/xtask` carries a
copy of both tables until the last importer moves, which is the bounded
duplication section 3.10 describes.

**Phase 2, image tools landed.** `cargo xtask image mkboot|mkfs` replaces
`tools/mkboot.rs` and `tools/mkfs.rs`: the trampoline and `kernel.bin` were
byte-identical to the old tool's, `just build-x86`, `just mkfs-x86` and
`just image x86` pass, and the stage1 compile of the tool (`rustc
tools/mkboot.rs` on every x86 image recipe) is gone. The superseded ports
(`mkinitramfs.rs`, `mkminixfs.rs`, `mkimg.rs`) go with them. `fetch-stage1`
and `verify-stage1` are still the phase's other half.

**Phase 2, contd.** `cargo xtask rust-config` replaces `tools/rust-config.py`,
which `just bootstrap` ran to write the fork's gitignored `rust/config.toml`.
The generated file is byte-identical to the old script's on the same host,
except for the header line that named the deleted script; the write keeps the
platform's line endings for the same reason (`Path.write_text` translated `\n`).
The target list, the `no-std` sections and the Python-style quoting are unit
tested, and so is the write against a temporary tree.

`cargo xtask fetch-stage1` replaces `tools/fetch-stage1.py`, so `just
fetch-stage1` and the two wasm CI jobs no longer need Python for it. The
download is `curl` (a named tool whose absence is a named error) and the
verification and unpacking are `sha2`, `tar` and `xz2`, which is why those
three are dependencies of the crate. `MINIXRS_STAGE1_BASE` still takes a URL or
a `file://` directory, which is how the fetcher was checked here: a fake release
was fetched, checksum-verified and extracted by both the script and the port,
with the same tree and the same marker (the script's `Path.write_text` CRLF
aside), and the already-installed, `--force`, checksum-mismatch and
missing-entry paths were exercised. `cargo xtask verify-stage1` replaces
`tools/verify-stage1.py` too, so phase 2 is done: it installs through the
fetcher, finds the sysroot's own `rust-lld`, and links `tools/std-hello.rs` for
the three minix targets, checking each ELF's `e_machine`. That smoke file names
`minix_libc`, which the script never passed, so it had been failing since std's
backtrace pulled the port's unwind stubs in; the port builds the rlib per
target with the same toolchain, as `cargo xtask build std-hello` does. It ran
here against this machine's stage1 with `MINIXRS_STAGE1_DEST` and `MINIXRS_LLD`
(the override names an lld for a source build, whose LLD is not in the sysroot),
linking x86-64, RISC-V and AArch64.

**Phase 1, contd.** `cargo xtask build gltriangle|gui` replaced two of the Mesa
clients. Both artifacts were byte-identical to the Python build's. That slice
also exposed a mistake worth recording: `tools/cdyn.py` was deleted in the
dynamic-linking commit while **eight** still-Python builders imported it, and
none of the gates noticed because they do not build those clients. It is
restored, and it stays until the last of the six remaining importers moves
(`build-gbmtest`, `build-wlgl`, `build-wldmabufgl`, `build-wlcompose`,
`build-composeprobe`, `build-eglprobe`). The rule in section 3.10 is the one
that was missed: delete what nothing imports, and check that with `grep`, not
with the test suite.

**Phase 1, done.** `cargo xtask build wlgl|wlcompose|wldmabufgl` replaced the
three Rust-half Wayland clients and `cargo xtask build gbmtest` the C-only GBM
probe. The Rust-half clients share one linker (`rust_client` in `build/mesa.rs`):
the userland rlib, the crt0's renamed entry, an optional C render object and the
DSOs that object calls. `build-composeprobe.py` and `build-eglprobe.py` had no
recipe left and went with them; `cdyn.py` lost its last importer and is gone.
The Mesa gates need `target/mesa-src`, which only a WSL `just build-mesa --build`
populates, so the artifacts were not rebuilt here; the local gate is
`cargo test --workspace`, `just check` and the command-line unit tests.

**Phase 3, started.** `cargo xtask fetch bash|mesa` replace
`tools/fetch-bash.py` and `tools/fetch-mesa.py`. The pinned values moved to
`tools/pins.ini`, which the xtask and the Python builders that still run under
WSL both read, so there is one source of truth; `cargo xtask pin bash|mesa`
derives the release tag from it, and the Python pin modules are thin readers that
keep the workflows' interface unchanged. Both fetchers were exercised end to end
against a `file://` release, including the idempotent path and the marker.
`mirror-bash`, `build-bash` and `build-mesa` stay Python for now: they drive the
WSL-only `cc` wrappers.

**Phase 5, started.** `cargo xtask probe read-hist` replaces
`tools/virtio_read_hist.py`, the first of the CI gate probes, and
`just test-read-cost-x86` calls it. Its output and its `--max` verdict match the
script's on a recorded 1159-request trace, exit status included; only the stdout
line endings differ, because Rust's `println!` does not translate `\n` the way
Python's text-mode stdout does on Windows.

**Phase 5, contd.** `cargo xtask probe rc|wlkey` replace `tools/rc_probe.py` and
`tools/wlkey_probe.py` on a shared harness: `src/guest.rs` spawns QEMU, merges its
stdout and stderr into one console transcript, paces typed bytes and kills the
child on drop, and `src/qmp.rs` is the minimal JSON-line client the key injection
needs (no JSON dependency: every command is a fixed string and the reply only has
to be classified). Both gates pass with the port, and `probe read-hist` moved to
`probe/read_hist.rs` unchanged.

`cargo xtask probe wlpointer` replaces `tools/wlpointer_probe.py` and
`cargo xtask probe wlfocus` replaces `tools/wlfocus_probe.py`, adding an absolute
pointer move to `qmp.rs` and a transcript read to `guest.rs`. `wlpointer` passes
with the port. `wlfocus` did not, and neither did its script: both dropped a
connection before the client was ready, as did `test-wloverlap-x86`, while a
single full-output surface (`test-wlx-x86`) and a 64x64 second window
(`test-wlpointer-x86`) passed. That was a tree bug, recorded as `KNOWN_ISSUES` 81
and the port matched the script's transcript exactly: the scene composer copied
each surface into a heap texture, and two output-sized copies exhausted the x86
guest. Composing now blits straight from the mapped pool, and both ported gates
pass (see the resolved item).

**Phase 5, done.** The rest of the probe family moved on the same harness. Three
helpers were extracted, because every port needed them: `probe/screen.rs` reads a
QMP screendump and waits on the size the PPM header promises (QEMU answers before
the file is complete); `probe/console.rs` is the paced, echo-checked typing that a
GL display needs, including `type_line_and_wait`, which splits "the line arrived"
from "the client answered"; and `guest.rs` grew anchored waits (`wait_for_after`,
`count`, `rfind`) so a prompt printed before a command cannot answer for it.

`cargo xtask probe wlmotion|wldemo|wlpanel` replace their scripts on the input
harness, with a mouse-button event added to `qmp.rs` for the panel's launcher.
`cargo xtask probe wlfull|wldmabuf|wldmabufgl|wlcompose|wlscene` take the whole
QEMU line from the recipe after `--`, because the devices and the disk image
differ per arch and per gate; the scripts took the same words positionally.
`cargo xtask probe dso-share` replaces the 570-line page-table probe, with the
walk, the shifts and the frame masks per arch as a row in a table, and the
arithmetic unit-tested against hand-built page tables. It reads symbols through
`toolchain::find_nm`, the same lookup `tools/lld.py` documents. Two measurements
changed with it. The console carries at most eight bytes inline, so a 13-byte
hold marker from each of two lives interleaves and is never contiguous; the
marker is now `dynhold\n`, eight bytes including its newline. And "maps the
object" is not enough to identify a life: `fb`'s heap shares `dso_base`, and an
exited slot keeps a nonzero `p_cr3`, so a candidate must be off `SLOT_FREE` and
its first page in the window must hold an ELF header. Both arches pass.

The recipes and CI name the new commands, and every reference in `README.md`, the
skills and `.notes/` moved with them. `tools/lld.py` stays for the Justfile's
parse-time variable; it lost the `dso_share_probe.py` importer, and the two that
remain (`stage1.py`, `rc-cost.py`) stay for good. The Mesa gates
(`wldmabufgl`, `wlcompose`, `wlscene`) were not run here: their clients still
build against `target/mesa-src`, which only a WSL `just build-mesa --build`
populates.

**Phase 4, landed.** `cargo xtask smoke` replaces `tools/smoke/feed.sh`. The driver owns the child, so it needs none of the shell's workarounds: no FIFO, no `timeout` binary, no status file, no subshell whose exit status is unavailable. It keeps the `.tsv` contract exactly: comments and blank lines, the tab separator (a line without one is an error), the whole-line expectation, the step's own byte anchor, the literal echo check for a paced send, and one shared budget that ends a second before the guest's timeout. The console is copied to the log as it arrives, so `_assert-qemu-log` and the image-boot recipes keep working. Every recipe that called `sh tools/smoke/feed.sh` now calls `cargo xtask smoke`, and the parser is tested against all 45 repository scenarios. `cargo xtask probe interactive-bash` replaces `tools/smoke/interactive_bash.py`, so phase 4 no longer has a script.

`cc-minix` and `cc-dso-minix` are **not** ported, and that is a decision
rather than a pending item. The reason is a host: `build-bash.py` and
`build-mesa.py` drive them from inside WSL (`build-mesa.py`'s meson cross file
names the script through `wsl_path`), where a Windows `xtask.exe` cannot run,
and a Linux `xtask` build in the same tree would share `.xtask/` with the
Windows one. Running Rust there needs a per-host dispatch, whose cheapest form
is a shell shim, and a shim is a worse trade than the Python it replaces: a
`cc` wrapper pays a process startup per object either way, and the tree would
grow a second entry point next to `cargo xtask`. These two are also the one
place the migration's premise does not hold, because the build they serve
already runs on a POSIX host. They stay Python, with `ccarch.py` and
`ccflags.py`.

### Phase 0 in detail, because it defines the shape of every later phase

Port the three static checkers first, for four reasons: they are pure file
scans over the tree (no toolchain, no QEMU, no network), they run in CI's
`host-tests` job on every push, they currently have no tests at all, and they
are the only part of `tools/` where "port it and test it" is a complete
statement of the work.

The parity method, which phases 1 to 3 reuse:

1. Move the logic into `crates/xtask/src/check/*.rs` as functions that take a
   root `&Path` and return a `Vec<Violation>`.
2. Write unit tests against `tempfile` trees: one fixture per rule, plus the
   two that matter most, a `physmap-ok:` exception and a `#[cfg(test)]` tail
   for the physmap checker, and a recipe missing a producer for the Justfile
   checker.
3. Prove the tests are not vacuous by reverting the rule and watching them
   fail, per the change workflow in `AGENTS.md`.
4. Until the Python is deleted, run both over the real tree and diff the
   output. This is a one-commit parity harness, not a permanent second
   implementation.
5. Delete the Python and the recipe's three lines become one.

Strengthening that should ride along: `justfile.rs` parses `BOOT_BINS` from
`crates/boot-image/src/manifest.rs`, so adding a boot binary without a producer
recipe fails the gate instead of the boot.

### Phase 4 is the one to be careful about

`feed.sh` is 277 lines of measured behaviour, and every comment in it is a
failure that was debugged. A Rust driver is genuinely better here (it owns the
child process, so no FIFO, no `timeout` binary, no status file, no subshell
whose exit status is unavailable), but it must reproduce:

- the prompt-before-send rule (input before the prompt is dropped);
- the per-step byte anchor, so an expectation printed by an earlier step does
  not satisfy this one (`silent-failure-traps` has the `pr` bisect that went
  green in five steps this way);
- whole-echo acceptance with a literal search, because a command is not a
  regex (`bash.tsv` step 3);
- the deliberate 15 s budget with the driver's verdict landing one second
  before the guest dies, so a stopped scenario names the step that stopped it.

Test it without QEMU: split "parse the scenario and decide whether a line
satisfies this step" from "spawn and read a guest", and unit-test the first
against the existing `.tsv` files and a recorded transcript. Then run every
`image-<arch>` recipe and `test-bash-<arch>` before landing.

## 7 What must not move

- **The C and asm sources and `tools/c-include/`.** They are the guest-side
  test programs and the C ABI contract; `xtask check headers` gates the
  contract (it was `tools/check-c-headers.py` before phase 0), and the C
  already builds on Windows without POSIX.
- **`tools/smoke/*.tsv`.** Four targets read them; they are data, and the
  `AGENTS.md` rule is that an expectation and a gate marker are contracts.
- **The linker scripts** (`minix-*.ld`). `.cargo/config.toml` points at them
  by path for three targets.
- **`tools/wasm-*`.** Node is the right runtime for a browser harness, and
  `publish.sh` boots the staged page; nothing in an `xtask` improves it.
- **`tools/probes/` (406 files).** Archaeology. It is not wired to anything,
  and moving it would create the impression it is maintained. If it needs
  anything, it needs a `README` saying where it came from, which is cheaper
  than either migrating or deleting it.

## 8 Testing the tooling itself

The migration's own rule: **no phase lands without a test that fails without
the phase.** Three kinds are cheap here and one is not:

- **Pure functions, host tests.** Path resolution against a fake `rust/build`
  tree, arch-name parsing, scenario line parsing, checker rules. These are why
  the scripts should be split into "decide" and "spawn" halves, the same
  pattern `AGENTS.md` asks for minix code.
- **`--dry-run` golden tests.** Command lines are the interface to clang,
  rustc, lld and QEMU. Asserting them against a golden file catches the flag
  that quietly disappears, which is exactly the class of bug a rewrite
  introduces.
- **Fixture trees.** The checkers' existing behaviour is defined by the repo;
  a `tempfile` tree per rule makes it explicit and lets the parity harness
  exist at all.
- **Not cheap: the QEMU gates.** They are evidence, not regression tests
  (`minix-testing`). Run them at the end of the relevant phase, once, and do
  not pretend they are part of the loop.

## 9 Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Behaviour drift in a ported script | A gate that passes for the wrong reason | Parity run over the real tree before deleting, plus the "revert the rule, watch the test fail" step |
| `cargo clean` breaks `bootstrap` on Windows | Bootstrap cannot complete | `--target-dir .xtask` in the alias (section 3.1), verified in phase 0 on Windows |
| Windows and Linux diverge in the new code | The current trap, recreated in Rust | Use `std::path` and `OsStr` throughout, never build a shell string, run `just test-linux` on host-side changes |
| `xtask` becomes a second build system | Two ways to do everything | One recipe body changes per command; the recipe name and arguments are fixed |
| A phase stalls half-done | Two implementations, silent drift | Deletions follow the last importer, so a shared module can outlive its first port; a phase that leaves one behind says which and why (section 3.10) |
| New dependencies sprout | A build dependency graph in an OS repo | Start with `clap` + `anyhow`; `xtask`'s optional modules and `xshell`/`sha2`/`tar` are added only by the phase that needs them |
| Host toolchain required earlier than today | `just bootstrap` on a python-only host | Verified in phase 2 (the `mkboot` move); `just` recipes already require `cargo` for everything except `bootstrap` itself |

## 10 Mechanical consequences

- **`.cargo/config.toml`** gains `[alias] xtask` and nothing else.
- **`.gitignore`** gains `.xtask/`.
- **`AGENTS.md`**: the gate table and the "Traps that keep catching us" list
  lose the MSYS entries only when phase 4 lands, not before. Do not pre-edit
  them: the rule is that a rule emerges from a validated pattern.
- **`Justfile`**: one body line per migrated recipe; the `check` recipe loses
  two of its dependencies over the migration.
- **CI**: unchanged until phase 5. The `python3` shim steps stay until no job
  runs a script; phases 0 to 3 remove them from `host-tests` and `kernel-tests`
  first.
- **Docs**: 22 files reference a `just` recipe and 23 reference a `tools/`
  path. Since recipe names do not change, the first number stays valid. The
  second needs a pass at the end of each phase for the files that phase
  deletes.
- **`.notes/`**: this document. Update the phase table as phases land, rather
  than writing a second plan.

## 11 Open questions

1. **Do we want `xshell`?** It makes the `running: ...` echo and the error
   context pleasant, and it is what `xtask`'s `process` feature wraps. If yes,
   take the crate. If no, the process layer is `std::process::Command` plus a
   small `run()` helper and the crate buys us only `repo` and `hash`.
2. **Does phase 3 belong in the plan at all?** `build-mesa.py` and
   `build-bash.py` are long because the *third-party* build is long, not
   because Python is. They are also the two scripts with the least to gain
   (they already run in WSL where there is no MSYS problem). Candidates for
   staying as-is permanently. If they do stay, `ccarch.py`, `mesa_pin.py`,
   `bash_pin.py` and `pin.py` stay with them, and that is an acceptable
   endpoint, not a failure.
3. **Do `stage1.py` and `lld.py` survive as a permanent exception?** Section
   3.2 recommends yes: 241 lines of pure path logic, needed before a toolchain
   exists, and embedded in the Justfile's parse-time evaluation. The cost is
   two Python files and two CI shims; the alternative is building `xtask`
   before `just --list` can run, which is worse. Worth a decision on the
   record rather than by default.
4. **Who owns the phase gate?** Each phase should name one command whose
   output is pasted into the commit, the way `.notes/test_gates.md` records
   its verdicts, so a reviewer can see it ran rather than infer it.
