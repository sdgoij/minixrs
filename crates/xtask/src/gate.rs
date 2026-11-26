//! One definition per QEMU gate.
//!
//! The Justfile keeps the build dependencies and the image construction; this
//! module owns everything about *running* a guest: the arch's QEMU command line,
//! the `.tsv` scenario it is driven with, the boot markers the console must
//! carry, and the verdict a passing run prints. A recipe that used to spell out
//! a QEMU line, an `_assert-qemu-log` call and an `echo` is now one
//! `cargo xtask gate <name> --arch <arch>` line.
//!
//! The gate table is data, so the command builders are the only code: each arch
//! has one canonical argument order (QEMU does not care about order) and a gate
//! selects an image, a memory size, a display, the devices it adds, and whether
//! a disk is attached. Anything a gate needs beyond that lives in its action.

use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::arch::{self, Arch};
use crate::guest::Guest;
use crate::probe;
use crate::smoke;

/// The oldest emulator the gates pass on. Older versions hang the aarch64 boot
/// suite behind a permanent IRQ storm, so an old emulator is named rather than
/// waited out.
const QEMU_MIN_VERSION: u32 = 11;

/// The QMP socket the command-taking probes connect to.
const QMP: &str = "tcp:127.0.0.1:4447,server=on,wait=off";

const ALL: &[&str] = &["x86", "riscv64", "aarch64"];
const X86_ONLY: &[&str] = &["x86"];
const X86_RISCV: &[&str] = &["x86", "riscv64"];

/// The shared userspace scenario. One file, read by this driver, the image boot
/// on every arch, and the wasm harness.
const SCENARIO: &str = "tools/smoke/scenario.tsv";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// The kernel with its initramfs and root filesystem embedded, loaded with
    /// `-kernel`.
    Embedded,
    /// The minimal boot image (`-kernel`) plus the root filesystem on a virtio
    /// disk. x86 additionally uses the trampoline artifact.
    System,
    /// The single-file release artifact `image-<arch>` produces.
    SelfContained,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Nographic,
    EglHeadless,
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub image: Image,
    pub memory: &'static str,
    pub display: Display,
    pub disk: bool,
    pub monitor_none: bool,
    pub display_none: bool,
    /// Keep the output device and drop the bochs display and the tablet, for the
    /// gates whose display *is* a virtio-gpu (the scanout re-point probes).
    pub gpu_only: bool,
    /// Extra `-device` arguments. Only x86 uses these; the RISC-V and AArch64
    /// lines already carry their virtio-gpu and keyboard.
    pub extras: &'static [&'static str],
    /// A `virtio_blk_handle_read` trace to write, for the read-cost gate.
    pub trace: Option<&'static str>,
}

const EMBEDDED_256: Layout = Layout {
    image: Image::Embedded,
    memory: "256M",
    display: Display::Nographic,
    disk: false,
    monitor_none: false,
    display_none: false,
    gpu_only: false,
    extras: &[],
    trace: None,
};

const SYSTEM_256: Layout = Layout {
    image: Image::System,
    disk: true,
    ..EMBEDDED_256
};

const SYSTEM_512: Layout = Layout {
    memory: "512M",
    ..SYSTEM_256
};

const SELF_CONTAINED_256: Layout = Layout {
    image: Image::SelfContained,
    ..EMBEDDED_256
};

const GPU: &[&str] = &["virtio-gpu-pci"];
const GPU_GL: &[&str] = &["virtio-gpu-gl-pci,blob=on"];

#[derive(Clone, Copy)]
pub enum Seconds {
    Fixed(u64),
    PerArch {
        x86: u64,
        riscv64: u64,
        aarch64: u64,
    },
}

const fn per(x86: u64, riscv64: u64, aarch64: u64) -> Seconds {
    Seconds::PerArch {
        x86,
        riscv64,
        aarch64,
    }
}

impl Seconds {
    fn get(self, arch: &Arch) -> u64 {
        match self {
            Seconds::Fixed(seconds) => seconds,
            Seconds::PerArch {
                x86,
                riscv64,
                aarch64,
            } => match arch.name {
                "x86" => x86,
                "riscv64" => riscv64,
                _ => aarch64,
            },
        }
    }
}

#[derive(Clone, Copy)]
pub enum Pace {
    None,
    Fixed(&'static str),
    /// No pacing on x86; a paced byte stream on the other arches. `fileread` is
    /// the one gate whose x86 console keeps up with a whole scenario line.
    ExceptX86(&'static str),
}

impl Pace {
    fn get(self, arch: &Arch) -> Option<&'static str> {
        match self {
            Pace::None => None,
            Pace::Fixed(pace) => Some(pace),
            Pace::ExceptX86(pace) => (arch.name != "x86").then_some(pace),
        }
    }
}

/// The lines a gate's console must carry. `X86` adds one marker that only the
/// x86 `fb` server prints.
#[derive(Clone, Copy)]
pub enum Markers {
    Same(&'static [&'static str]),
    X86(&'static [&'static str], &'static str),
}

impl Markers {
    fn get(self, arch: &Arch) -> Vec<&'static str> {
        match self {
            Markers::Same(lines) => lines.to_vec(),
            Markers::X86(lines, extra) => {
                let mut all = lines.to_vec();
                if arch.name == "x86" {
                    all.push(extra);
                }
                all
            }
        }
    }
}

/// A follow-up probe that reads the data the guest produced.
#[derive(Clone, Copy)]
pub enum After {
    ReadHist {
        trace: &'static str,
        max: u64,
    },
}

#[derive(Clone, Copy)]
pub struct Smoke {
    pub scenario: &'static str,
    pub pace: Pace,
    pub markers: Markers,
    /// Tool names whose `name:` output means a command failed. The coreutils
    /// gate is the one user; the check is the shell grep it replaces.
    pub forbid_prefixes: &'static [&'static str],
    pub forbid_exact: &'static [&'static str],
    pub after: Option<After>,
}

const DEFAULT_SMOKE: Smoke = Smoke {
    scenario: SCENARIO,
    pace: Pace::None,
    markers: Markers::Same(&[]),
    forbid_prefixes: &[],
    forbid_exact: &[],
    after: None,
};

#[derive(Clone, Copy)]
pub struct Stage {
    pub scenario: &'static str,
    pub seconds: u64,
    pub pace: Pace,
    pub markers: Markers,
}

#[derive(Clone, Copy)]
pub enum Probe {
    /// A probe that builds its own x86 QEMU command.
    Own(&'static str),
    /// A probe that also takes the arch and memory to boot.
    OwnArch(&'static str),
    /// A probe that takes the whole QEMU line from the caller.
    Gated(&'static str),
    /// The repeated-exec leak probe, with its measured defaults.
    ExecLoop,
    InteractiveBash,
    DsoShare,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SuiteKind {
    Test,
    Boot,
}

#[derive(Clone, Copy)]
pub enum Action {
    Smoke(Smoke),
    Boot {
        markers: &'static [&'static str],
    },
    Probe {
        probe: Probe,
        qmp: bool,
    },
    Suite {
        kind: SuiteKind,
        marker: &'static str,
    },
    Sequence(&'static [Stage]),
}

/// A build step a gate runs before it boots a guest. The recipe lives in the
/// Justfile, so the build logic stays where the project keeps it.
#[derive(Clone, Copy)]
pub enum Prepare {
    /// `just <recipe> <arch>`.
    Arch(&'static str),
    /// `just <recipe>`.
    Exact(&'static str),
}

const BUILD: &[Prepare] = &[Prepare::Arch("build")];
const SYSTEM_IMAGE: &[Prepare] = &[Prepare::Arch("system-image")];

#[derive(Clone, Copy)]
pub struct Gate {
    pub name: &'static str,
    pub arches: &'static [&'static str],
    pub seconds: Seconds,
    pub layout: Layout,
    /// The layout the non-x86 arches use, where it differs. `wlfull` is the one
    /// gate whose x86 artifact is embedded while its other arches boot the split
    /// system image.
    pub non_x86_layout: Option<Layout>,
    /// The build recipe(s) that produce the artifact the gate boots.
    pub prepare: &'static [Prepare],
    pub action: Action,
    /// Printed on success. `{arch}`, `{image}` and `{qemu}` are substituted.
    pub verdict: &'static str,
}

impl Gate {
    fn layout_for(self, arch: &Arch) -> Layout {
        if arch.name == "x86" {
            self.layout
        } else {
            self.non_x86_layout.unwrap_or(self.layout)
        }
    }

    fn with_non_x86(self, layout: Layout) -> Gate {
        Gate {
            non_x86_layout: Some(layout),
            ..self
        }
    }
}

fn g(
    name: &'static str,
    arches: &'static [&'static str],
    seconds: Seconds,
    layout: Layout,
    action: Action,
    verdict: &'static str,
) -> Gate {
    Gate {
        name,
        arches,
        seconds,
        layout,
        non_x86_layout: None,
        prepare: match layout.image {
            Image::System => SYSTEM_IMAGE,
            Image::Embedded | Image::SelfContained => BUILD,
        },
        action,
        verdict,
    }
}

fn smoke(scenario: &'static str) -> Action {
    Action::Smoke(Smoke {
        scenario,
        ..DEFAULT_SMOKE
    })
}

fn smoke_markers(scenario: &'static str, markers: &'static [&'static str]) -> Action {
    Action::Smoke(Smoke {
        scenario,
        markers: Markers::Same(markers),
        ..DEFAULT_SMOKE
    })
}

fn smoke_paced(
    scenario: &'static str,
    pace: Pace,
    markers: &'static [&'static str],
) -> Action {
    Action::Smoke(Smoke {
        scenario,
        pace,
        markers: Markers::Same(markers),
        ..DEFAULT_SMOKE
    })
}

/// The coreutils tools whose `name: ...` output means the command failed.
const COREUTILS_TOOLS: &[&str] = &[
    "touch", "env", "ln", "chgrp", "chown", "groups", "id", "logname", "whoami", "install",
    "mkfifo", "chroot", "coreutils", "ls", "dir", "vdir", "cp", "mv", "expr", "link", "hostid",
    "nice", "date", "dd", "tty", "dircolors", "csplit", "du", "kill", "uname", "arch", "nproc",
    "hostname", "timeout", "more",
];

const COREUTILS_FAILURES: &[&str] = &[
    "RM-STILL-THERE",
    "RMR-STILL-THERE",
    "CHMOD-X-BAD",
    "CHMOD-ENOENT-BAD",
    "INSTALL-M-BAD",
];

/// Every gate, in the order the recipes read.
fn table() -> Vec<Gate> {
    let gates = vec![
        // ---------- the split system image, every arch ----------
        g(
            "system-image",
            ALL,
            per(40, 60, 60),
            SYSTEM_256,
            smoke_markers(SCENARIO, &["wserver: ready"]),
            "system-image: the userspace scenario ran in the split system image ({arch})",
        ),
        // ---------- the self-contained release artifact ----------
        g(
            "image",
            ALL,
            Seconds::Fixed(15),
            SELF_CONTAINED_256,
            Action::Smoke(Smoke {
                markers: Markers::X86(&["wserver: ready"], "fb: gpu3d none"),
                ..DEFAULT_SMOKE
            }),
            "done: {image} — boot it with {qemu} -nographic -m 256M -kernel <it>",
        ),
        // ---------- framebuffer and memory gates, every arch ----------
        g(
            "fbmmap",
            ALL,
            per(40, 60, 60),
            SYSTEM_256,
            smoke("tools/smoke/fbmmap.tsv"),
            "fbmmap: a userland mmap of /dev/fb aliases the live framebuffer ({arch})",
        ),
        g(
            "writeback",
            ALL,
            per(40, 60, 60),
            SYSTEM_256,
            smoke_paced("tools/smoke/writeback.tsv", Pace::Fixed("0.003"), &[]),
            "writeback: a MAP_SHARED file store reached the file ({arch})",
        ),
        g(
            "fork",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/fork.tsv"),
            "fork: both directions of the copy held ({arch})",
        ),
        g(
            "waitid",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/waitid.tsv"),
            "waitid: status decoded, P_ALL named the child, unsupported states refused ({arch})",
        ),
        g(
            "winprobe",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/winprobe.tsv"),
            "winprobe: no user VA outside a region is reachable ({arch})",
        ),
        g(
            "procfs",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/procfs.tsv"),
            "procfs: /proc is mounted, serves hz, uptime and loadavg, resolves /proc/self and /proc/self/exe, names the running program, and lists its entries once each ({arch})",
        ),
        g(
            "burst",
            ALL,
            per(30, 60, 60),
            EMBEDDED_256,
            smoke_paced("tools/smoke/burst.tsv", Pace::Fixed("0.003"), &[]),
            "burst: every long line reached the shell and ran ({arch})",
        ),
        g(
            "ttytimeout",
            ALL,
            per(30, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/ttytimeout.tsv"),
            "ttytimeout: a MIN/TIME read returned after its deadline ({arch})",
        ),
        g(
            "pipeblock",
            ALL,
            per(30, 40, 40),
            EMBEDDED_256,
            smoke("tools/smoke/pipeblock.tsv"),
            "pipeblock: a pipe read waited for its writer ({arch})",
        ),
        g(
            "threads",
            ALL,
            per(30, 40, 40),
            EMBEDDED_256,
            smoke("tools/smoke/threads.tsv"),
            "threads: four kernel threads shared the process and were joined ({arch})",
        ),
        g(
            "fileread",
            ALL,
            per(40, 120, 120),
            EMBEDDED_256,
            smoke_paced("tools/smoke/fileread.tsv", Pace::ExceptX86("0.003"), &[]),
            "fileread: a runtime-written file's indirect blocks read back ({arch})",
        ),
        g(
            "coreutils-minix",
            ALL,
            per(300, 480, 480),
            Layout {
                memory: "512M",
                ..EMBEDDED_256
            },
            Action::Smoke(Smoke {
                scenario: "tools/smoke/coreutils-minix.tsv",
                pace: Pace::Fixed("0.003"),
                forbid_prefixes: COREUTILS_TOOLS,
                forbid_exact: COREUTILS_FAILURES,
                ..DEFAULT_SMOKE
            }),
            "coreutils-minix: the core scenario and ls, cp, mv, expr, link, hostid, nice, date, dd, tty, dircolors, csplit, du, uname, arch, nproc, hostname, timeout, nohup, more and kill ran ({arch})",
        ),
        // ---------- x86's C and build gates ----------
        g(
            "cabi",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/cabi.tsv"),
            "cabi: the C socket, pipe2, group and mmap ABIs held ({arch})",
        ),
        // ---------- x86 userspace syscall gates ----------
        g(
            "mmapanon",
            X86_ONLY,
            Seconds::Fixed(20),
            EMBEDDED_256,
            smoke("tools/smoke/mmapanon.tsv"),
            "mmapanon: MAP_SHARED|MAP_ANONYMOUS is shared across fork ({arch})",
        ),
        g(
            "memfd",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/memfd.tsv"),
            "memfd: one object mapped twice and across a fork, read/write agreeing ({arch})",
        ),
        g(
            "uds",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/uds.tsv"),
            "uds: socketpair, bind/listen/connect/accept and SCM_RIGHTS/SCM_CREDS passing ({arch})",
        ),
        g(
            "pty",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/pty.tsv"),
            "pty: a poll on a pty master is woken by the slave's write ({arch})",
        ),
        g(
            "select",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/select.tsv"),
            "select/poll: pipe readiness and real timeouts ({arch})",
        ),
        g(
            "eventfd",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/eventfd.tsv"),
            "eventfd: counter semantics and a cross-process wake ({arch})",
        ),
        g(
            "timerfd",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/timerfd.tsv"),
            "timerfd: one-shot, periodic, absolute and disarmed timers ({arch})",
        ),
        g(
            "epoll",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/epoll.tsv"),
            "epoll: persistent interest set, level-triggered readiness, cross-process wake ({arch})",
        ),
        g(
            "wayland",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/wayland.tsv"),
            "wayland: registry + sync handshake over /dev/uds ({arch})",
        ),
        g(
            "wlshm",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/wlshm.tsv"),
            "wlshm: wl_shm buffer committed and presented to /dev/fb ({arch})",
        ),
        g(
            "wloverlap",
            X86_ONLY,
            Seconds::Fixed(60),
            EMBEDDED_256,
            smoke_markers("tools/smoke/wloverlap.tsv", &["wloverlap: PASS"]),
            "wloverlap: the lower window's commit left the window above it intact ({arch})",
        ),
        g(
            "wlx",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/wlx.tsv"),
            "wlx: xdg_shell toplevel configured, acked and presented ({arch})",
        ),
        g(
            "wlxd",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/wlxd.tsv"),
            "wlxd: damage and the pointer's cursor ({arch})",
        ),
        g(
            "wlxe",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/wlxe.tsv"),
            "wlxe: layer-shell panel, xdg_popup and client-side decoration ({arch})",
        ),
        g(
            "link",
            X86_ONLY,
            Seconds::Fixed(20),
            EMBEDDED_256,
            smoke("tools/smoke/link.tsv"),
            "link: a hard link and a rename carried their names ({arch})",
        ),
        g(
            "symlink",
            X86_ONLY,
            Seconds::Fixed(20),
            EMBEDDED_256,
            smoke("tools/smoke/symlink.tsv"),
            "symlink: a client created a link in /tmp, read its target back through the grant, and got the three refusals ({arch})",
        ),
        g(
            "readlink",
            X86_ONLY,
            Seconds::Fixed(20),
            EMBEDDED_256,
            smoke("tools/smoke/readlink.tsv"),
            "readlink: a client read the target of the symlink the image ships, and the refusals a read has to give ({arch})",
        ),
        g(
            "long-path",
            X86_ONLY,
            Seconds::Fixed(30),
            EMBEDDED_256,
            smoke("tools/smoke/long-path.tsv"),
            "long-path: every length resolved ({arch})",
        ),
        g(
            "coreutils-wedge",
            &["x86", "aarch64"],
            per(20, 0, 30),
            EMBEDDED_256,
            smoke("tools/smoke/coreutils-wedge.tsv"),
            "coreutils-wedge: no child was left wedged ({arch})",
        ),
        // ---------- the render node and its clients ----------
        g(
            "gpu3d-nogl",
            X86_ONLY,
            Seconds::Fixed(20),
            Layout {
                extras: GPU,
                ..EMBEDDED_256
            },
            smoke_markers(
                "tools/smoke/drminfo.tsv",
                &["fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"],
            ),
            "gpu3d: a device the host gave no GL to was reported as no 3D by the fb server and by a client opening its render node ({arch})",
        ),
        g(
            "drmmap",
            X86_ONLY,
            Seconds::Fixed(20),
            Layout {
                extras: GPU,
                ..EMBEDDED_256
            },
            smoke_markers(
                "tools/smoke/drmmap.tsv",
                &["fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"],
            ),
            "drmmap: a client created a render node memory object, mapped it twice, found one set of frames, saw a second object be other memory, and closed it ({arch})",
        ),
        g(
            "drmprime",
            X86_ONLY,
            Seconds::Fixed(20),
            Layout {
                extras: GPU,
                ..EMBEDDED_256
            },
            smoke_markers(
                "tools/smoke/drmprime.tsv",
                &["fb: gpu3d device, no VIRTIO_GPU_F_VIRGL"],
            ),
            "drmprime: a client exported a render node object to a dma-buf descriptor and imported it back, and the bytes survived ({arch})",
        ),
        g(
            "drmgl",
            X86_ONLY,
            Seconds::Fixed(40),
            Layout {
                display: Display::EglHeadless,
                extras: GPU_GL,
                ..EMBEDDED_256
            },
            smoke_paced(
                "tools/smoke/drmgl.tsv",
                Pace::Fixed("0.02"),
                &["wserver: ready", "wlserver: ready", "capsets 2 blob 1"],
            ),
            "drmgl: a client was driven into a guest with a GL display, and the node answered its capset query with the device's blobs on ({arch})",
        ),
        g(
            "gpu3d",
            X86_ONLY,
            Seconds::Fixed(30),
            Layout {
                display: Display::EglHeadless,
                extras: GPU_GL,
                ..EMBEDDED_256
            },
            Action::Boot {
                markers: &[
                    "wserver: ready",
                    "wlserver: ready",
                    "fb: gpu3d virgl capset",
                    "xfer 16384/16384",
                    "submit 16384/16384",
                ],
            },
            "gpu3d: the render node negotiated virgl, answered the capset query, and round-tripped a texture ({arch})",
        ),
        // ---------- Mesa's clients ----------
        g(
            "gltriangle",
            &["x86", "riscv64", "aarch64"],
            Seconds::Fixed(180),
            SYSTEM_512,
            smoke("tools/smoke/gltriangle.tsv"),
            "gltriangle: the GLES2 triangle rendered through surfaceless EGL on softpipe ({arch})",
        ),
        g(
            "gltriangle-virgl",
            X86_ONLY,
            Seconds::Fixed(180),
            Layout {
                display: Display::EglHeadless,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            smoke_paced(
                "tools/smoke/gltriangle.tsv",
                Pace::Fixed("0.02"),
                &["gltriangle: pass", "gltriangle: GL_RENDERER=virgl"],
            ),
            "gltriangle-virgl: the GLES2 triangle rendered through surfaceless EGL on the render node, via virgl ({arch})",
        ),
        g(
            "wlgl",
            X86_ONLY,
            Seconds::Fixed(60),
            Layout {
                display: Display::EglHeadless,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            smoke_paced(
                "tools/smoke/wlgl.tsv",
                Pace::Fixed("0.02"),
                &["wlgl: GL_RENDERER=virgl", "wlgl: PASS"],
            ),
            "wlgl: a GL surface rendered on the render node, committed through the compositor, and read back from /dev/fb ({arch})",
        ),
        g(
            "gui",
            X86_ONLY,
            Seconds::Fixed(120),
            SYSTEM_512,
            smoke_markers("tools/smoke/gui.tsv", &["gui: PASS"]),
            "gui: a quad and a line of text from the in-house renderer, presented through the compositor, and read back from /dev/fb ({arch})",
        ),
        g(
            "doom",
            ALL,
            per(180, 600, 600),
            SYSTEM_256,
            smoke_paced(
                "tools/smoke/doom.tsv",
                Pace::Fixed("0.02"),
                &["doom: PASS"],
            ),
            "doom: the shareware IWAD runs, the first frame's pixels are read back out of /dev/fb ({arch})",
        ),
        g(
            "doom-kbd",
            ALL,
            per(240, 600, 600),
            SYSTEM_256,
            Action::Probe {
                probe: Probe::OwnArch("doom-kbd"),
                qmp: false,
            },
            "doom-kbd: a host key reaches Doom through the input server's /dev/kbd ring ({arch})",
        ),
        g(
            "doom-save",
            ALL,
            per(300, 900, 900),
            SYSTEM_256,
            Action::Probe {
                probe: Probe::OwnArch("doom-save"),
                qmp: false,
            },
            "doom-save: Doom writes .savegame/doomsav0.dsg and it survives a reboot ({arch})",
        ),
        g(
            "gbmtest",
            X86_ONLY,
            Seconds::Fixed(120),
            Layout {
                display: Display::EglHeadless,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            smoke_paced(
                "tools/smoke/gbmtest.tsv",
                Pace::Fixed("0.02"),
                &["gbmtest: dev=1 bo=1 w=64 h=64 import=0"],
            ),
            "gbmtest: a GBM buffer object created on the render node exported to a dma-buf and imported back ({arch})",
        ),
        // ---------- the read-cost trace ----------
        g(
            "read-cost",
            X86_ONLY,
            Seconds::Fixed(40),
            Layout {
                disk: true,
                monitor_none: true,
                trace: Some("target/test-read-cost.trace"),
                ..EMBEDDED_256
            },
            Action::Smoke(Smoke {
                scenario: "tools/smoke/read-cost.tsv",
                pace: Pace::Fixed("0.003"),
                after: Some(After::ReadHist {
                    trace: "target/test-read-cost.trace",
                    max: 1200,
                }),
                ..DEFAULT_SMOKE
            }),
            "read-cost: per-command read cost within the ceiling ({arch})",
        ),
        // ---------- the dynamic-linking gate ----------
        g(
            "dynlink",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/dyn.tsv"),
            "dynlink: /bin/dynhello printed what only the shared objects contain, before and after a fork and an exec, and /bin/dynclib ran against libc.so ({arch})",
        ),
        g(
            "cdyn",
            X86_ONLY,
            Seconds::Fixed(40),
            EMBEDDED_256,
            smoke("tools/smoke/cdyn.tsv"),
            "cdyn: a program built by the recipe ran against the shipped libc.so ({arch})",
        ),
        g(
            "c-direct",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/cdirect.tsv"),
            "c-direct: clang + LLD linked /bin/cdirect against libc.a with no rustc, and it ran ({arch})",
        ),
        g(
            "bash",
            ALL,
            per(40, 60, 60),
            EMBEDDED_256,
            smoke("tools/smoke/bash.tsv"),
            "bash: every step of tools/smoke/bash.tsv answered ({arch})",
        ),
        // ---------- the in-guest toolchain ----------
        g(
            "toolchain",
            X86_ONLY,
            Seconds::Fixed(3600),
            Layout {
                memory: "1024M",
                ..SYSTEM_256
            },
            smoke_paced("tools/smoke/toolchain.tsv", Pace::Fixed("0.01"), &[]),
            "toolchain: the guest's clang compiled and the guest's lld linked hello.c, and it ran ({arch})",
        ),
        g(
            "rustc",
            X86_ONLY,
            Seconds::Fixed(3600),
            Layout {
                memory: "2048M",
                ..SYSTEM_256
            },
            smoke_paced("tools/smoke/rustc.tsv", Pace::Fixed("0.01"), &[]),
            "rustc: the guest's rustc compiled and linked a Rust program, and it ran ({arch})",
        ),
        g(
            "tblgen",
            X86_ONLY,
            Seconds::Fixed(5400),
            Layout {
                memory: "2048M",
                ..SYSTEM_256
            },
            Action::Sequence(&[
                Stage {
                    scenario: "tools/smoke/tblgen-build.tsv",
                    seconds: 5400,
                    pace: Pace::Fixed("0.01"),
                    markers: Markers::Same(&[]),
                },
                Stage {
                    scenario: "tools/smoke/tblgen-run.tsv",
                    seconds: 300,
                    pace: Pace::Fixed("0.01"),
                    markers: Markers::Same(&[]),
                },
            ]),
            "tblgen: the guest compiled and linked llvm-min-tblgen, and it ran in a fresh boot ({arch})",
        ),
        // ---------- probes that own their QEMU line ----------
        g(
            "rc",
            X86_ONLY,
            Seconds::Fixed(150),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("rc"),
                qmp: false,
            },
            "rc: /etc/rc's commands ran at boot before the shell ({arch})",
        ),
        g(
            "fb",
            X86_ONLY,
            Seconds::Fixed(60),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("fb"),
                qmp: false,
            },
            "fb: the frame is the red/green/blue boot pattern ({arch})",
        ),
        g(
            "exec-loop",
            X86_ONLY,
            Seconds::Fixed(300),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::ExecLoop,
                qmp: false,
            },
            "exec-loop: 100 execs left the free-page and region counts flat ({arch})",
        ),
        g(
            "alloc",
            X86_ONLY,
            Seconds::Fixed(60),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::OwnArch("alloc"),
                qmp: false,
            },
            "alloc: the region count returned to its baseline ({arch})",
        ),
        g(
            "wlkey",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wlkey"),
                qmp: false,
            },
            "wlkey: keymap served and wl_seat keyboard events reach a client ({arch})",
        ),
        g(
            "wlfocus",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wlfocus"),
                qmp: false,
            },
            "wlfocus: two clients, one focus, and the keys follow it ({arch})",
        ),
        g(
            "wlmotion",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wlmotion"),
                qmp: false,
            },
            "wlmotion: the pointer moved in steps to where the device landed ({arch})",
        ),
        g(
            "wlpointer",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wlpointer"),
                qmp: false,
            },
            "wlpointer: the focus followed the pointer between two windows ({arch})",
        ),
        g(
            "wldemo",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wldemo"),
                qmp: false,
            },
            "wldemo: the field, the list and the button took their keys ({arch})",
        ),
        g(
            "wlpanel",
            X86_ONLY,
            Seconds::Fixed(120),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::Own("wlpanel"),
                qmp: false,
            },
            "wlpanel: the launcher took a click on the layer surface ({arch})",
        ),
        // ---------- probes that take the QEMU line ----------
        g(
            "wlfull",
            ALL,
            Seconds::Fixed(300),
            Layout {
                gpu_only: true,
                monitor_none: true,
                display_none: true,
                ..EMBEDDED_256
            },
            Action::Probe {
                probe: Probe::Gated("wlfull"),
                qmp: true,
            },
            "wlfull: a whole-output wl_shm frame was scanned out of the client's own buffer ({arch})",
        )
        .with_non_x86(Layout {
            monitor_none: true,
            display_none: true,
            ..SYSTEM_256
        }),
        g(
            "wldmabuf",
            X86_ONLY,
            Seconds::Fixed(300),
            Layout {
                gpu_only: true,
                monitor_none: true,
                display_none: true,
                ..EMBEDDED_256
            },
            Action::Probe {
                probe: Probe::Gated("wldmabuf"),
                qmp: true,
            },
            "wldmabuf: a render-node object was handed over as a linux-dmabuf buffer and scanned out with no copy ({arch})",
        ),
        g(
            "wldmabufgl",
            X86_ONLY,
            Seconds::Fixed(300),
            Layout {
                display: Display::EglHeadless,
                gpu_only: true,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            Action::Probe {
                probe: Probe::Gated("wldmabufgl"),
                qmp: true,
            },
            "wldmabufgl: the GL triangle's render target was the dma-buf wl_buffer, scanned out of the client's object with no compositor copy ({arch})",
        ),
        g(
            "wlcompose",
            X86_ONLY,
            Seconds::Fixed(300),
            Layout {
                display: Display::EglHeadless,
                gpu_only: true,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            Action::Probe {
                probe: Probe::Gated("wlcompose"),
                qmp: false,
            },
            "wlcompose: two GL surfaces were composed on the render node by a helper in another process, and the frame read back holds them ({arch})",
        ),
        g(
            "wlscene",
            X86_ONLY,
            Seconds::Fixed(300),
            Layout {
                display: Display::EglHeadless,
                gpu_only: true,
                extras: GPU_GL,
                ..SYSTEM_512
            },
            Action::Probe {
                probe: Probe::Gated("wlscene"),
                qmp: true,
            },
            "wlscene: the compositor composed two dma-buf surfaces on the GPU and scanned the frame out ({arch})",
        ),
        g(
            "interactive-bash",
            ALL,
            per(40, 60, 60),
            SYSTEM_256,
            Action::Probe {
                probe: Probe::InteractiveBash,
                qmp: false,
            },
            "interactive-bash: an interactive bash accepted a typed line ({arch})",
        ),
        // ---------- the shared-page-table probe ----------
        g(
            "dso-share",
            X86_RISCV,
            Seconds::Fixed(180),
            EMBEDDED_256,
            Action::Probe {
                probe: Probe::DsoShare,
                qmp: false,
            },
            "dso-share: two processes mapping one object shared its read-only frames ({arch})",
        ),
        // ---------- the kernel suites ----------
        g(
            "qemu",
            ALL,
            Seconds::Fixed(60),
            EMBEDDED_256,
            Action::Suite {
                kind: SuiteKind::Test,
                marker: "ALL TESTS PASSED",
            },
            "qemu: the in-kernel suite passed ({arch})",
        ),
        g(
            "boot",
            ALL,
            Seconds::Fixed(60),
            EMBEDDED_256,
            Action::Suite {
                kind: SuiteKind::Boot,
                marker: "ALL TESTS PASSED",
            },
            "boot: the boot suite passed ({arch})",
        ),
    ];

    gates.into_iter().map(with_prepare).collect()
}

/// The gates whose artifact is not simply `build <arch>` or `system-image <arch>`
/// name the recipe that produces it. Everything else uses the layout default.
fn with_prepare(mut gate: Gate) -> Gate {
    gate.prepare = match gate.name {
        "image" => &[Prepare::Arch("prepare-image")],
        "bash" => &[Prepare::Arch("prepare-bash")],
        "dynlink" => &[Prepare::Arch("prepare-dynlink")],
        "dso-share" => &[Prepare::Arch("prepare-dso-share")],
        "c-direct" => &[Prepare::Arch("prepare-c-direct")],
        "cdyn" => &[Prepare::Exact("prepare-cdyn-x86")],
        "cabi" => &[Prepare::Arch("build-c-hello"), Prepare::Arch("build")],
        "gltriangle" => &[Prepare::Arch("prepare-gltriangle")],
        "gltriangle-virgl" => &[Prepare::Exact("prepare-gltriangle-virgl-x86")],
        "wlgl" => &[Prepare::Exact("prepare-wlgl-x86")],
        "gui" => &[Prepare::Exact("prepare-gui-x86")],
        "doom" => &[Prepare::Arch("prepare-doom")],
        "doom-kbd" => &[Prepare::Arch("prepare-doom")],
        "doom-save" => &[Prepare::Arch("prepare-doom")],
        "gbmtest" => &[Prepare::Exact("prepare-gbmtest-x86")],
        "wldmabufgl" => &[Prepare::Exact("prepare-wldmabufgl-x86")],
        "wlcompose" => &[Prepare::Exact("prepare-wlcompose-x86")],
        "wlscene" => &[Prepare::Exact("prepare-wlscene-x86")],
        "interactive-bash" => &[Prepare::Arch("build-bash"), Prepare::Arch("system-image")],
        "toolchain" | "tblgen" => &[
            Prepare::Exact("toolchain-root-x86"),
            Prepare::Arch("system-image"),
        ],
        "rustc" => &[
            Prepare::Exact("rustc-root-x86"),
            Prepare::Arch("system-image"),
        ],
        "wlfull" => &[Prepare::Arch("prepare-wlfull")],
        "qemu" => &[
            Prepare::Arch("prepare-build-test"),
            Prepare::Arch("prepare-mkfs"),
        ],
        "boot" => &[
            Prepare::Arch("prepare-build-boot"),
            Prepare::Arch("prepare-mkfs"),
        ],
        _ => gate.prepare,
    };
    gate
}

/// The gate `name` defines, if there is one.
pub fn find(name: &str) -> Option<Gate> {
    table().into_iter().find(|gate| gate.name == name)
}

/// Every gate, one line each: its name, the arches it runs on, and its verdict.
pub fn list(out: &mut impl Write) -> io::Result<bool> {
    for gate in table() {
        writeln!(
            out,
            "{:<20} {:<20} {}",
            gate.name,
            gate.arches.join(","),
            gate.verdict
        )?;
    }
    Ok(true)
}

/// The gates the old `just test-arches` ran, in the same order and per arch.
const ARCH_GATES: &[(&str, &str)] = &[
    ("qemu", "x86"),
    ("boot", "x86"),
    ("fork", "x86"),
    ("waitid", "x86"),
    ("cabi", "x86"),
    ("mmapanon", "x86"),
    ("memfd", "x86"),
    ("uds", "x86"),
    ("writeback", "x86"),
    ("winprobe", "x86"),
    ("procfs", "x86"),
    ("burst", "x86"),
    ("ttytimeout", "x86"),
    ("pipeblock", "x86"),
    ("threads", "x86"),
    ("eventfd", "x86"),
    ("timerfd", "x86"),
    ("epoll", "x86"),
    ("read-cost", "x86"),
    ("coreutils-minix", "x86"),
    ("fileread", "x86"),
    ("c-direct", "x86"),
    ("qemu", "riscv64"),
    ("boot", "riscv64"),
    ("fork", "riscv64"),
    ("waitid", "riscv64"),
    ("writeback", "riscv64"),
    ("winprobe", "riscv64"),
    ("procfs", "riscv64"),
    ("burst", "riscv64"),
    ("ttytimeout", "riscv64"),
    ("pipeblock", "riscv64"),
    ("threads", "riscv64"),
    ("coreutils-minix", "riscv64"),
    ("fileread", "riscv64"),
    ("c-direct", "riscv64"),
    ("qemu", "aarch64"),
    ("boot", "aarch64"),
    ("fork", "aarch64"),
    ("waitid", "aarch64"),
    ("writeback", "aarch64"),
    ("winprobe", "aarch64"),
    ("procfs", "aarch64"),
    ("burst", "aarch64"),
    ("ttytimeout", "aarch64"),
    ("pipeblock", "aarch64"),
    ("threads", "aarch64"),
    ("coreutils-wedge", "aarch64"),
    ("coreutils-minix", "aarch64"),
    ("fileread", "aarch64"),
    ("c-direct", "aarch64"),
];

/// Run every gate the arch suites cover, stopping at the first failure.
pub fn run_arches(root: &Path) -> io::Result<bool> {
    for (name, arch) in ARCH_GATES {
        println!("=== gate {name} {arch} ===");
        if !run(root, name, arch, None, false)? {
            return Ok(false);
        }
    }
    println!("gate-arches: every arch gate passed");
    Ok(true)
}

/// Run one gate for `arch_token`.
pub fn run(
    root: &Path,
    name: &str,
    arch_token: &str,
    timeout: Option<u64>,
    dry_run: bool,
) -> io::Result<bool> {
    let arch = arch::resolve(arch_token)?;
    let gate = find(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown gate {name:?}; run `cargo xtask gate --list`"),
        )
    })?;
    if !gate.arches.contains(&arch.name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "gate {} does not run on {}; it runs on {}",
                gate.name,
                arch.name,
                gate.arches.join(", ")
            ),
        ));
    }
    let seconds = timeout.unwrap_or_else(|| gate.seconds.get(arch));

    if dry_run {
        return dry_run_gate(root, gate, arch);
    }

    run_prepare(root, gate, arch)?;
    verify_qemu(arch)?;

    let passed = match gate.action {
        Action::Smoke(spec) => run_smoke(root, gate, arch, seconds, spec, "")?,
        Action::Boot { markers } => run_boot(root, gate, arch, seconds, markers)?,
        Action::Probe { probe, qmp } => run_probe(root, gate, arch, seconds, probe, qmp)?,
        Action::Suite { kind, marker } => run_suite(root, gate, arch, seconds, kind, marker)?,
        Action::Sequence(stages) => run_sequence(root, gate, arch, stages)?,
    };
    if !passed {
        return Ok(false);
    }

    let image = image_path(root, arch, gate.layout_for(arch).image);
    println!("{}", expand(gate.verdict, arch, root, &image));
    Ok(true)
}

fn dry_run_gate(root: &Path, gate: Gate, arch: &'static Arch) -> io::Result<bool> {
    let layout = gate.layout_for(arch);
    for &step in gate.prepare {
        println!("{}", prepare_line(step, arch));
    }
    match gate.action {
        Action::Smoke(spec) => {
            let command = standard_command(root, arch, layout, false);
            println!("{}", shell_line(&command));
            println!("# scenario {}", spec.scenario);
        }
        Action::Boot { .. } => {
            let command = standard_command(root, arch, layout, false);
            println!("{}", shell_line(&command));
        }
        Action::Probe {
            probe: Probe::Gated(_),
            qmp,
        } => {
            let command = standard_command(root, arch, layout, qmp);
            println!("{}", shell_line(&command));
        }
        Action::Probe {
            probe: Probe::InteractiveBash,
            ..
        } => {
            let command = standard_command(root, arch, layout, false);
            println!("{}", shell_line(&command));
        }
        Action::Probe { probe, .. } => {
            println!("cargo xtask probe {}", probe_name(probe));
        }
        Action::Suite { kind, .. } => {
            let command = suite_command(root, arch, kind);
            println!("{}", shell_line(&command));
        }
        Action::Sequence(stages) => {
            let command = standard_command(root, arch, layout, false);
            for stage in stages {
                println!("{}", shell_line(&command));
                println!("# scenario {}", stage.scenario);
            }
        }
    }
    Ok(true)
}

fn probe_name(probe: Probe) -> &'static str {
    match probe {
        Probe::Own(name) | Probe::OwnArch(name) | Probe::Gated(name) => name,
        Probe::ExecLoop => "exec-loop",
        Probe::InteractiveBash => "interactive-bash",
        Probe::DsoShare => "dso-share",
    }
}

fn run_smoke(
    root: &Path,
    gate: Gate,
    arch: &'static Arch,
    seconds: u64,
    spec: Smoke,
    suffix: &str,
) -> io::Result<bool> {
    let layout = gate.layout_for(arch);
    prepare_image(root, arch, layout.image)?;
    let log = log_path(root, gate.name, arch, suffix);
    let mut command = standard_command(root, arch, layout, false);
    if let Some(trace) = layout.trace {
        let _ = fs::remove_file(root.join(trace));
    }
    let pace = parse_pace(spec.pace.get(arch))?;
    let scenario = root.join(spec.scenario);
    if !smoke::run_with(&log, seconds, &scenario, pace, &mut command)? {
        return Ok(false);
    }
    if !assert_log(
        &log,
        &spec.markers.get(arch),
        spec.forbid_prefixes,
        spec.forbid_exact,
    )? {
        return Ok(false);
    }
    if let Some(after) = spec.after
        && !run_after(root, after)?
    {
        return Ok(false);
    }
    Ok(true)
}

fn run_sequence(root: &Path, gate: Gate, arch: &'static Arch, stages: &[Stage]) -> io::Result<bool> {
    for (index, stage) in stages.iter().enumerate() {
        let spec = Smoke {
            scenario: stage.scenario,
            pace: stage.pace,
            markers: stage.markers,
            forbid_prefixes: &[],
            forbid_exact: &[],
            after: None,
        };
        let suffix = format!("-{}", index + 1);
        if !run_smoke(root, gate, arch, stage.seconds, spec, &suffix)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn run_boot(
    root: &Path,
    gate: Gate,
    arch: &'static Arch,
    seconds: u64,
    markers: &[&str],
) -> io::Result<bool> {
    let layout = gate.layout_for(arch);
    prepare_image(root, arch, layout.image)?;
    let log = log_path(root, gate.name, arch, "");
    let mut command = standard_command(root, arch, layout, false);
    let guest = Guest::spawn_tee(&mut command, &log)?;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    for marker in markers {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !guest.wait_for(marker.as_bytes(), remaining) {
            eprintln!("!! the guest did not print {:?}", marker);
            eprintln!("{}", guest.tail(3000));
            return Ok(false);
        }
    }
    if reported_failures(&guest) {
        eprintln!("!! the guest reported failures");
        eprintln!("{}", guest.tail(3000));
        return Ok(false);
    }
    Ok(true)
}

fn run_suite(
    root: &Path,
    gate: Gate,
    arch: &'static Arch,
    seconds: u64,
    kind: SuiteKind,
    marker: &str,
) -> io::Result<bool> {
    let log = log_path(root, gate.name, arch, "");
    let mut command = suite_command(root, arch, kind);
    let guest = Guest::spawn_tee(&mut command, &log)?;

    // x86's kernel suite prints a different completion marker, so the table's
    // marker is the common one and the x86 rewrite is here.
    let wanted = if arch.name == "x86" && kind == SuiteKind::Test {
        "-- done --"
    } else {
        marker
    };
    let ok = guest.wait_for(wanted.as_bytes(), Duration::from_secs(seconds));
    if !ok {
        eprintln!("!! no {wanted:?} in {}", log.display());
        eprintln!("{}", guest.tail(3000));
        return Ok(false);
    }
    if reported_failures(&guest) {
        eprintln!("!! failures reported in {}", log.display());
        eprintln!("{}", guest.tail(3000));
        return Ok(false);
    }
    Ok(true)
}

fn run_probe(
    root: &Path,
    gate: Gate,
    arch: &'static Arch,
    seconds: u64,
    probe: Probe,
    qmp: bool,
) -> io::Result<bool> {
    prepare_image(root, arch, gate.layout_for(arch).image)?;
    match probe {
        Probe::Own(name) => match name {
            "rc" => probe::rc::run(root),
            "fb" => probe::fb::run(root),
            "wlkey" => probe::wlkey::run(root),
            "wlfocus" => probe::wlfocus::run(root),
            "wlmotion" => probe::wlmotion::run(root),
            "wlpointer" => probe::wlpointer::run(root),
            "wldemo" => probe::wldemo::run(root),
            "wlpanel" => probe::wlpanel::run(root),
            other => Err(unknown_probe(other)),
        },
        Probe::OwnArch(name) => match name {
            "alloc" => probe::alloc::run(root, arch.name, gate.layout_for(arch).memory),
            "doom-kbd" => probe::doom::run(root, arch.name),
            "doom-save" => probe::doom::run_save(root, arch.name),
            other => Err(unknown_probe(other)),
        },
        Probe::ExecLoop => probe::exec_loop::run(
            root,
            &probe::exec_loop::Options {
                arch: arch.name,
                memory: gate.layout_for(arch).memory,
                execs: 100,
                sample_every: 10,
                command: "hello",
                marker: "pid=",
                complete: "threadstd",
            },
        ),
        Probe::DsoShare => probe::dso_share::run(
            root,
            arch.name,
            gate.layout_for(arch).memory,
            "/bin/dynclib",
        ),
        Probe::InteractiveBash => {
            let log = log_path(root, gate.name, arch, "");
            let mut command = standard_command(root, arch, gate.layout_for(arch), false);
            probe::interactive_bash::run(&log, seconds, &mut command)
        }
        Probe::Gated(name) => {
            let command = standard_command(root, arch, gate.layout_for(arch), qmp);
            let words = command_words(&command);
            match name {
                "wlfull" => probe::wlfull::run(root, &words),
                "wldmabuf" => probe::wldmabuf::run(root, &words),
                "wldmabufgl" => probe::wldmabufgl::run(root, &words),
                "wlcompose" => probe::wlcompose::run(root, &words),
                "wlscene" => probe::wlscene::run(root, &words),
                other => Err(unknown_probe(other)),
            }
        }
    }
}

fn unknown_probe(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("gate table names unknown probe {name:?}"),
    )
}

fn run_after(root: &Path, after: After) -> io::Result<bool> {
    match after {
        After::ReadHist { trace, max } => {
            probe::read_hist::read_hist(&root.join(trace), Some(max))
        }
    }
}

/// The image a gate boots, and the file `prepare_image` has to produce first.
fn image_path(root: &Path, arch: &Arch, image: Image) -> PathBuf {
    let images = root.join("target").join("images").join(arch.triple);
    match image {
        Image::Embedded if arch.name == "x86" => images.join("minix-x86.elf"),
        Image::Embedded => root
            .join("target")
            .join(arch.triple)
            .join("release")
            .join(format!("kernel-boot-{}", arch.name)),
        Image::System => images.join(format!("minix-{}-boot.elf", arch.name)),
        Image::SelfContained => images.join(format!("minix-{}.elf", arch.name)),
    }
}

fn disk_path(root: &Path, arch: &Arch) -> PathBuf {
    root.join("target")
        .join("images")
        .join(arch.triple)
        .join("disk.img")
}

/// Copy the artifact a gate boots into the path the QEMU line names.
///
/// x86's self-contained and embedded artifacts are the trampoline the image
/// build produced; the other arches' release artifacts are copied only for
/// `image-<arch>`, whose recipes used to do that copy themselves.
fn prepare_image(root: &Path, arch: &Arch, image: Image) -> io::Result<()> {
    match image {
        Image::Embedded | Image::SelfContained if arch.name == "x86" => {
            let source = root.join("target").join("trampoline.elf");
            copy_image(
                &source,
                &image_path(root, arch, image),
                "run `just build-x86` first",
            )
        }
        Image::SelfContained => {
            let source = root
                .join("target")
                .join(arch.triple)
                .join("release")
                .join(format!("kernel-boot-{}", arch.name));
            copy_image(
                &source,
                &image_path(root, arch, image),
                &format!("run `just build-{}` first", arch.name),
            )
        }
        _ => Ok(()),
    }
}

fn copy_image(source: &Path, destination: &Path, hint: &str) -> io::Result<()> {
    if !source.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is missing; {hint}", source.display()),
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn log_path(root: &Path, name: &str, arch: &Arch, suffix: &str) -> PathBuf {
    root.join("target")
        .join(format!("test-{name}{suffix}-{}.log", arch.name))
}

/// The standard gate command: one canonical order per arch.
fn standard_command(root: &Path, arch: &Arch, layout: Layout, qmp: bool) -> Command {
    let image = image_path(root, arch, layout.image);
    match arch.name {
        "x86" => x86_command(root, arch, layout, &image, qmp),
        "riscv64" => riscv_command(root, arch, layout, &image, qmp),
        _ => aarch_command(root, arch, layout, &image, qmp),
    }
}

fn add_disk_x86(command: &mut Command, root: &Path, arch: &Arch) {
    let disk = disk_path(root, arch);
    command.args([
        "-drive",
        &format!(
            "if=none,id=disk0,file={},format=raw,cache=writethrough",
            disk.display()
        ),
        "-device",
        "virtio-blk-pci,disable-legacy=on,drive=disk0",
    ]);
}

fn add_disk_virtio(command: &mut Command, root: &Path, arch: &Arch) {
    let disk = disk_path(root, arch);
    command.args([
        "-drive",
        &format!(
            "if=none,id=disk0,file={},format=raw,cache=writethrough",
            disk.display()
        ),
        "-device",
        "virtio-blk-device,drive=disk0",
    ]);
}

fn add_qmp(command: &mut Command) {
    command.arg("-qmp").arg(QMP);
}

fn x86_command(
    root: &Path,
    arch: &Arch,
    layout: Layout,
    image: &Path,
    qmp: bool,
) -> Command {
    let mut command = Command::new("qemu-system-x86_64");
    match layout.display {
        Display::Nographic => {
            command.arg("-nographic");
        }
        Display::EglHeadless => {
            command.args(["-display", "egl-headless", "-serial", "stdio"]);
        }
    }
    if layout.monitor_none {
        command.args(["-monitor", "none"]);
    }
    if layout.display_none {
        command.args(["-display", "none"]);
    }
    command.args(["-m", layout.memory, "-no-reboot", "-vga", "none"]);
    if layout.gpu_only {
        command.args(["-device", "virtio-gpu-pci"]);
    } else {
        command.args(["-device", "bochs-display,id=fb0"]);
    }
    command.arg("-kernel").arg(image);
    if layout.disk {
        add_disk_x86(&mut command, root, arch);
    }
    command.args([
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-pci,disable-legacy=on,netdev=net0",
    ]);
    if !layout.gpu_only {
        command.args(["-device", "virtio-tablet-pci,display=fb0"]);
    }
    for extra in layout.extras {
        command.args(["-device", extra]);
    }
    if let Some(trace) = layout.trace {
        command
            .args(["-trace", "enable=virtio_blk_handle_read", "-D"])
            .arg(root.join(trace));
    }
    if qmp {
        add_qmp(&mut command);
    }
    command
}

fn riscv_command(
    root: &Path,
    arch: &Arch,
    layout: Layout,
    image: &Path,
    qmp: bool,
) -> Command {
    let mut command = Command::new("qemu-system-riscv64");
    command.args(["-machine", "virt", "-m", layout.memory, "-nographic"]);
    if layout.monitor_none {
        command.args(["-monitor", "none"]);
    }
    if layout.display_none {
        command.args(["-display", "none"]);
    }
    command.args(["-global", "virtio-mmio.force-legacy=off"]);
    if layout.disk {
        add_disk_virtio(&mut command, root, arch);
    }
    command.args([
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-device,netdev=net0",
        "-device",
        "virtio-gpu-device",
        "-device",
        "virtio-keyboard-device",
    ]);
    command.arg("-kernel").arg(image);
    if qmp {
        add_qmp(&mut command);
    }
    command
}

fn aarch_command(
    root: &Path,
    arch: &Arch,
    layout: Layout,
    image: &Path,
    qmp: bool,
) -> Command {
    let mut command = Command::new("qemu-system-aarch64");
    command.args([
        "-machine",
        "virt",
        "-cpu",
        "cortex-a57",
        "-m",
        layout.memory,
        "-nographic",
        "-no-reboot",
    ]);
    if layout.monitor_none {
        command.args(["-monitor", "none"]);
    }
    if layout.display_none {
        command.args(["-display", "none"]);
    }
    command.args(["-global", "virtio-mmio.force-legacy=off"]);
    if layout.disk {
        add_disk_virtio(&mut command, root, arch);
    }
    command.args([
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-device,netdev=net0",
        "-device",
        "virtio-gpu-device",
        "-device",
        "virtio-keyboard-device",
    ]);
    command.arg("-kernel").arg(image);
    if qmp {
        add_qmp(&mut command);
    }
    command
}

/// The kernel and boot suites, whose images and devices differ from the gates'.
fn suite_command(root: &Path, arch: &Arch, kind: SuiteKind) -> Command {
    match arch.name {
        "x86" => {
            let (trampoline, binary) = match kind {
                SuiteKind::Test => ("kernel-test-trampoline.elf", "kernel-test.bin"),
                SuiteKind::Boot => ("kernel-boot-trampoline.elf", "kernel-boot.bin"),
            };
            let mut command = Command::new("qemu-system-x86_64");
            command.args(["-nographic", "-m", "256M", "-no-reboot"]);
            command
                .arg("-kernel")
                .arg(root.join("target").join(trampoline));
            command.args([
                "-device",
                &format!(
                    "loader,file={},addr=0x200000",
                    root.join("target").join(binary).display()
                ),
                "-device",
                "isa-debug-exit",
                "-monitor",
                "none",
            ]);
            add_disk_x86(&mut command, root, arch);
            command
        }
        _ => {
            let program = if arch.name == "riscv64" {
                "qemu-system-riscv64"
            } else {
                "qemu-system-aarch64"
            };
            let mut command = Command::new(program);
            if arch.name == "riscv64" {
                command.args(["-machine", "virt", "-m", "256M", "-nographic"]);
            } else {
                command.args([
                    "-machine",
                    "virt",
                    "-cpu",
                    "cortex-a57",
                    "-m",
                    "256M",
                    "-nographic",
                    "-no-reboot",
                ]);
            }
            if kind == SuiteKind::Boot {
                command.args(["-global", "virtio-mmio.force-legacy=off"]);
                add_disk_virtio(&mut command, root, arch);
                command.args(["-device", "virtio-gpu-device", "-device", "virtio-keyboard-device"]);
            }
            let suffix = match kind {
                SuiteKind::Test => "test",
                SuiteKind::Boot => "boot",
            };
            let image = root
                .join("target")
                .join(arch.triple)
                .join("release")
                .join(format!("kernel-boot-{}-{suffix}", arch.name));
            command.arg("-kernel").arg(image);
            command
        }
    }
}

fn run_prepare(root: &Path, gate: Gate, arch: &Arch) -> io::Result<()> {
    for &step in gate.prepare {
        let line = prepare_line(step, arch);
        println!("preparing: {line}");
        let mut command = Command::new("just");
        command.current_dir(root);
        match step {
            Prepare::Arch(recipe) => {
                command.arg(recipe).arg(arch.name);
            }
            Prepare::Exact(recipe) => {
                command.arg(recipe);
            }
        }
        let status = command.status().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot run `just` (the build front door): {error}"),
            )
        })?;
        if !status.success() {
            return Err(io::Error::other(format!("the build step `{line}` failed")));
        }
    }
    Ok(())
}

fn prepare_line(step: Prepare, arch: &Arch) -> String {
    match step {
        Prepare::Arch(recipe) => format!("just {recipe} {}", arch.name),
        Prepare::Exact(recipe) => format!("just {recipe}"),
    }
}

fn command_words(command: &Command) -> Vec<String> {
    let mut words = vec![command.get_program().to_string_lossy().into_owned()];
    words.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    words
}

fn shell_line(command: &Command) -> String {
    command_words(command)
        .iter()
        .map(|word| {
            if word.contains(' ') {
                format!("{word:?}")
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn qemu_program(arch: &Arch) -> &'static str {
    match arch.name {
        "x86" => "qemu-system-x86_64",
        "riscv64" => "qemu-system-riscv64",
        _ => "qemu-system-aarch64",
    }
}

fn verify_qemu(arch: &Arch) -> io::Result<()> {
    let program = qemu_program(arch);
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot run {program}: {error} (is QEMU on PATH?)"),
            )
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    let major = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("QEMU emulator version "))
        .and_then(|rest| rest.split('.').next())
        .and_then(|major| major.parse::<u32>().ok());
    match major {
        Some(major) if major >= QEMU_MIN_VERSION => Ok(()),
        Some(major) => Err(io::Error::other(format!(
            "{program} is version {major}.x; the gates need QEMU {QEMU_MIN_VERSION} or newer, because older emulators hang the aarch64 boot suite"
        ))),
        None => Err(io::Error::other(format!(
            "could not read {program}'s version from `{program} --version`"
        ))),
    }
}

fn parse_pace(value: Option<&str>) -> io::Result<Duration> {
    let Some(value) = value else {
        return Ok(Duration::ZERO);
    };
    let seconds: f64 = value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the gate's pace is {value}, not a number of seconds"),
        )
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the gate's pace is {value}, not a positive number of seconds"),
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn reported_failures(guest: &Guest) -> bool {
    guest.with_transcript(|out| String::from_utf8_lossy(out).contains("FAILURES:"))
}

/// The log assertions `_assert-qemu-log` made, plus the coreutils gate's grep.
fn assert_log(
    log: &Path,
    markers: &[&str],
    forbid_prefixes: &[&str],
    forbid_exact: &[&str],
) -> io::Result<bool> {
    let bytes = fs::read(log)?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("FAILURES:") {
        eprintln!("!! failures reported in {}", log.display());
        eprintln!("{}", tail(&text, 30));
        return Ok(false);
    }
    for marker in markers {
        if !text.contains(marker) {
            eprintln!(
                "!! no {marker:?} in {} - the guest did not reach that point. Tail:",
                log.display()
            );
            eprintln!("{}", tail(&text, 30));
            return Ok(false);
        }
    }
    for line in text.lines() {
        if forbid_exact.contains(&line) {
            eprintln!("!! a tool left a failure marker: {line}");
            return Ok(false);
        }
        if let Some((prefix, _)) = line.split_once(':')
            && forbid_prefixes.contains(&prefix)
        {
            eprintln!("!! a tool reported an error: {line}");
            return Ok(false);
        }
    }
    Ok(true)
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

fn expand(text: &str, arch: &Arch, root: &Path, image: &Path) -> String {
    let image = image.strip_prefix(root).unwrap_or(image);
    text.replace("{arch}", arch.machine)
        .replace("{image}", &image.to_string_lossy())
        .replace("{qemu}", qemu_program(arch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_arch_has_the_gate_it_names() {
        for gate in table() {
            for name in gate.arches {
                assert!(
                    arch::resolve(name).is_ok(),
                    "gate {} names unknown arch {name}",
                    gate.name
                );
            }
            assert!(!gate.name.is_empty());
            assert!(!gate.verdict.is_empty(), "gate {} has no verdict", gate.name);
        }
    }

    #[test]
    fn a_gate_name_is_unique() {
        let mut names: Vec<&str> = table().iter().map(|gate| gate.name).collect();
        names.sort_unstable();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped, "two gates share a name");
    }

    #[test]
    fn an_unknown_gate_is_not_found() {
        assert!(find("does-not-exist").is_none());
        assert!(find("fork").is_some());
    }

    #[test]
    fn the_x86_embedded_line_has_the_bochs_display_and_the_image() {
        let gate = find("fork").unwrap();
        let arch = arch::resolve("x86").unwrap();
        let command = standard_command(Path::new("root"), arch, gate.layout, false);
        let words = command_words(&command);
        assert!(words.contains(&"bochs-display,id=fb0".to_string()));
        assert!(words.contains(&"virtio-tablet-pci,display=fb0".to_string()));
        assert!(words.iter().any(|word| word.ends_with("minix-x86.elf")));
        assert!(words.contains(&"-no-reboot".to_string()));
    }

    #[test]
    fn a_system_gate_attaches_a_disk_and_uses_the_boot_image() {
        let gate = find("fbmmap").unwrap();
        let arch = arch::resolve("riscv64").unwrap();
        let command = standard_command(Path::new("root"), arch, gate.layout, false);
        let words = command_words(&command);
        assert!(words
            .iter()
            .any(|word| word.ends_with("minix-riscv64-boot.elf")));
        assert!(words.contains(&"virtio-blk-device,drive=disk0".to_string()));
        assert!(!words.contains(&"-no-reboot".to_string()));
    }

    #[test]
    fn an_egl_display_replaces_nographic() {
        let gate = find("drmgl").unwrap();
        let arch = arch::resolve("x86").unwrap();
        let command = standard_command(Path::new("root"), arch, gate.layout, false);
        let words = command_words(&command);
        assert!(words.contains(&"egl-headless".to_string()));
        assert!(!words.contains(&"-nographic".to_string()));
        assert!(words.contains(&"virtio-gpu-gl-pci,blob=on".to_string()));
    }

    #[test]
    fn the_gpu_only_layout_drops_the_bochs_display_and_the_tablet() {
        let gate = find("wldmabuf").unwrap();
        let arch = arch::resolve("x86").unwrap();
        let command = standard_command(Path::new("root"), arch, gate.layout, true);
        let words = command_words(&command);
        assert!(!words.contains(&"bochs-display,id=fb0".to_string()));
        assert!(!words.contains(&"virtio-tablet-pci,display=fb0".to_string()));
        assert!(words.contains(&"virtio-gpu-pci".to_string()));
        assert!(words.iter().any(|word| word.contains("server=on,wait=off")));
    }

    #[test]
    fn wlfull_on_x86_makes_the_virtio_gpu_the_output() {
        let gate = find("wlfull").unwrap();
        let arch = arch::resolve("x86").unwrap();
        let layout = gate.layout_for(arch);
        assert!(layout.gpu_only, "wlfull must not put a bochs display on x86");
        let words = command_words(&standard_command(Path::new("root"), arch, layout, true));
        assert!(words.contains(&"virtio-gpu-pci".to_string()));
        assert!(!words.contains(&"bochs-display,id=fb0".to_string()));
        assert!(!words.contains(&"virtio-tablet-pci,display=fb0".to_string()));
    }

    #[test]
    fn the_x86_suite_has_the_debug_exit_and_the_loader() {
        let arch = arch::resolve("x86").unwrap();
        let command = suite_command(Path::new("root"), arch, SuiteKind::Boot);
        let words = command_words(&command);
        assert!(words.contains(&"isa-debug-exit".to_string()));
        assert!(words.iter().any(|word| word.contains("kernel-boot.bin")));
        assert!(words
            .iter()
            .any(|word| word.ends_with("kernel-boot-trampoline.elf")));
    }

    #[test]
    fn the_image_gate_asserts_the_x86_degrade_line_and_the_others_do_not() {
        let markers = Markers::X86(&["wserver: ready"], "fb: gpu3d none");
        assert!(markers
            .get(arch::resolve("x86").unwrap())
            .contains(&"fb: gpu3d none"));
        assert!(!markers
            .get(arch::resolve("aarch64").unwrap())
            .contains(&"fb: gpu3d none"));
    }

    #[test]
    fn a_verdict_expands_its_arch_and_image() {
        let arch = arch::resolve("x86").unwrap();
        let text = expand(
            "done: {image} via {qemu} ({arch})",
            arch,
            Path::new("root"),
            Path::new("root/target/images/x86_64-pc-minix/minix-x86.elf"),
        );
        assert!(text.contains("(x86_64)"));
        assert!(text.contains("qemu-system-x86_64"));
        assert!(text.contains("minix-x86.elf"));
    }

    #[test]
    fn a_gate_only_names_arches_it_declares() {
        let gate = find("gpu3d").unwrap();
        assert_eq!(gate.arches, X86_ONLY);
        let error = run(Path::new("root"), "gpu3d", "aarch64", None, true).unwrap_err();
        assert!(error.to_string().contains("does not run on aarch64"));
    }

    #[test]
    fn every_arch_gate_names_a_gate_that_runs_on_that_arch() {
        for (name, arch) in ARCH_GATES {
            let gate = find(name).unwrap_or_else(|| panic!("gate {name} is missing"));
            assert!(
                gate.arches.contains(arch),
                "gate {name} does not run on {arch}"
            );
        }
    }

    #[test]
    fn the_coreutils_gate_checks_its_tool_prefixes() {
        assert!(COREUTILS_TOOLS.contains(&"touch"));
        assert!(COREUTILS_TOOLS.contains(&"more"));
        assert!(COREUTILS_FAILURES.contains(&"RM-STILL-THERE"));
    }

    fn temp_log(name: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "minixrs-gate-{}-{name}.log",
            std::process::id()
        ));
        fs::write(&path, contents).expect("the temporary log is writable");
        path
    }

    #[test]
    fn a_marker_the_log_carries_passes() {
        let log = temp_log("marker", "booting\nwserver: ready\n");
        assert!(assert_log(&log, &["wserver: ready"], &[], &[]).unwrap());
        let _ = fs::remove_file(log);
    }

    #[test]
    fn a_marker_the_log_lacks_fails() {
        let log = temp_log("no-marker", "booting\n");
        assert!(!assert_log(&log, &["wserver: ready"], &[], &[]).unwrap());
        let _ = fs::remove_file(log);
    }

    #[test]
    fn a_failures_line_fails_every_gate() {
        let log = temp_log("failures", "FAILURES: two checks\n");
        assert!(!assert_log(&log, &[], &[], &[]).unwrap());
        let _ = fs::remove_file(log);
    }

    #[test]
    fn a_tool_prefix_before_a_colon_is_a_failure_and_a_bare_name_is_not() {
        let failed = temp_log("tool-error", "touch: cannot touch 'x': Permission denied\n");
        assert!(!assert_log(&failed, &[], COREUTILS_TOOLS, COREUTILS_FAILURES).unwrap());
        let _ = fs::remove_file(failed);

        let prose = temp_log("prose", "the touch command ran\n");
        assert!(assert_log(&prose, &[], COREUTILS_TOOLS, COREUTILS_FAILURES).unwrap());
        let _ = fs::remove_file(prose);
    }

    #[test]
    fn a_failure_marker_line_is_exact() {
        let log = temp_log("marker-exact", "RM-STILL-THERE\n");
        assert!(!assert_log(&log, &[], COREUTILS_TOOLS, COREUTILS_FAILURES).unwrap());
        let _ = fs::remove_file(log);
    }
}
