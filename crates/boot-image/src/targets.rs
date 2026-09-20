//! Per-target path resolution for the MINIX userland builds.
//!
//! The userland and server binaries live in the **shared** cargo target
//! directory (`target/<triple>/release/`), built by the top-level Just
//! recipes. `crates/kernel/build.rs` and the CLI wrappers read from there;
//! nobody invokes cargo from inside a build script (that would deadlock on
//! the cargo lock, and a disjoint dir would throw away the build cache).

use std::path::{Path, PathBuf};

/// A MINIX userland build target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildTarget {
    /// Short arch name used by the CLIs ("x86_64", "riscv64", "aarch64").
    pub arch: &'static str,
    /// Cargo `--target` value (in-tree rust fork triple).
    pub spec: &'static str,
    /// Cargo target output sub-directory (same as the triple).
    pub out_dir: &'static str,
}

pub const X86_64: BuildTarget = BuildTarget {
    arch: "x86_64",
    spec: "x86_64-pc-minix",
    out_dir: "x86_64-pc-minix",
};

pub const RISCV64: BuildTarget = BuildTarget {
    arch: "riscv64",
    spec: "riscv64gc-unknown-minix",
    out_dir: "riscv64gc-unknown-minix",
};

pub const AARCH64: BuildTarget = BuildTarget {
    arch: "aarch64",
    spec: "aarch64-unknown-minix",
    out_dir: "aarch64-unknown-minix",
};

/// wasm32, whose "userland build" is not a set of executables but a set of *modules*.
///
/// It resolves by arch name only, and deliberately not by rustc triple: `target_from_rustc_target`
/// answers "which userland binaries does an image for this target hold", and the answer for wasm32
/// is *none* — §7.3's point is that a wasm kernel carries no userland, and the host supplies the
/// modules. Adding it there would make `crates/kernel/build.rs` look for `target/wasm32-minix/release/*`
/// and embed them in an initramfs this port does not use. The CLI's question is the other one —
/// "which image can I build for this arch" — and that has an answer, because the *image* is where
/// a wasm program comes from (§7.2's step 4).
pub const WASM32: BuildTarget = BuildTarget {
    arch: "wasm32",
    spec: "wasm32-minix",
    out_dir: "wasm32-minix",
};

/// Resolve a `BuildTarget` from a rustc `TARGET` env value (as seen by a
/// build script), or `None` for host/foreign targets.
pub fn target_from_rustc_target(rustc_target: &str) -> Option<BuildTarget> {
    [X86_64, RISCV64, AARCH64]
        .into_iter()
        .find(|&t| rustc_target == t.out_dir || rustc_target == t.spec)
}

/// Resolve a `BuildTarget` from a short arch name ("x86_64", "riscv64",
/// "aarch64", "wasm32").
pub fn target_from_arch(arch: &str) -> Option<BuildTarget> {
    [X86_64, RISCV64, AARCH64, WASM32]
        .into_iter()
        .find(|&t| arch == t.arch)
}

/// The shared cargo release directory holding the built userland/servers
/// binaries for `t` (e.g. `target/x86_64-pc-minix/release`).
pub fn release_dir(t: &BuildTarget, workspace: &Path) -> PathBuf {
    workspace.join("target").join(t.out_dir).join("release")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_resolution() {
        assert_eq!(target_from_rustc_target("x86_64-pc-minix"), Some(X86_64));
        assert_eq!(
            target_from_rustc_target("aarch64-unknown-minix"),
            Some(AARCH64)
        );
        assert_eq!(
            target_from_rustc_target("riscv64gc-unknown-minix"),
            Some(RISCV64)
        );
        assert_eq!(target_from_rustc_target("x86_64-pc-windows-msvc"), None);
        assert_eq!(target_from_arch("aarch64"), Some(AARCH64));
        assert_eq!(target_from_arch("mips"), None);
        // wasm32 builds an image but is not a rustc-triple match: the kernel's build script
        // asks the triple question, and for wasm32 the answer is "no userland to embed".
        assert_eq!(target_from_arch("wasm32"), Some(WASM32));
        assert_eq!(target_from_rustc_target("wasm32-minix"), None);
        assert_eq!(target_from_rustc_target("wasm32-unknown-unknown"), None);
    }

    #[test]
    fn release_dir_is_per_target() {
        let workspace = Path::new("/repo");
        assert_eq!(
            release_dir(&X86_64, workspace),
            PathBuf::from("/repo/target/x86_64-pc-minix/release")
        );
        assert_eq!(
            release_dir(&AARCH64, workspace),
            PathBuf::from("/repo/target/aarch64-unknown-minix/release")
        );
    }
}
