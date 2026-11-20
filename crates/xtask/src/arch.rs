//! The minix userland C targets, and what a C build has to be told to name one.
//!
//! Three drivers need the same answers (`cc-minix`, the smoke binaries, and the
//! bash build), so the table lives here rather than three times over. What the
//! answers are is the point: the *minix* triple is what the rustc rlib is built
//! for, and the clang *machine* is what the headers and the object code are
//! compiled for. They are the same machine, one per arch, and the link step is
//! where the two are made to agree.

use std::io;
use std::path::{Path, PathBuf};

/// One minix userland target.
#[derive(Debug)]
pub struct Arch {
    /// The short name the Justfile and the recipes use (`x86`, `riscv64`, ...).
    pub name: &'static str,

    /// The minix triple: what the rlib is built for and what rustc links for.
    pub triple: &'static str,

    /// The machine clang is told (`--target=<machine>-unknown-none`), which is
    /// also the suffix of the arch's `crt0`.
    pub machine: &'static str,

    /// Whether `-mno-red-zone` means anything here. It is an x86_64 and AArch64
    /// flag; RISC-V has no red zone, and clang only warns that it is unused.
    red_zone: bool,

    /// Flags this machine needs beyond the common set, where clang's own default
    /// disagrees with what the minix target's rustc emits.
    ///
    /// RISC-V is the case: `riscv64-unknown-none` defaults to the *soft-float*
    /// ABI, while the fork's `riscv64gc-unknown-minix` spec is `Lp64d`, and lld
    /// refuses to link objects whose `EF_RISCV_FLOAT_ABI` differ. That is also a
    /// correctness question, not just a link error: `printf("%f")`'s double
    /// would cross the boundary in a register one side does not use.
    extra_cflags: &'static [&'static str],
}

impl Arch {
    pub fn clang_target(&self) -> String {
        format!("{}-unknown-none", self.machine)
    }

    pub fn crt0(&self, root: &Path) -> PathBuf {
        root.join("tools").join(format!("crt0-{}.S", self.machine))
    }

    pub fn libc_deps(&self, root: &Path) -> PathBuf {
        root.join("target").join(self.triple).join("release").join("deps")
    }

    /// The flags every freestanding compile of C for this target needs.
    ///
    /// `-fno-pic` matches the kernel's static relocation model, and the red-zone
    /// flag matches the fork spec's `disable_redzone`. What keeps the compile
    /// off the host's headers is [`crate::cc::compile_flags`], not this list.
    pub fn base_cflags(&self) -> Vec<String> {
        let mut flags = vec![
            format!("--target={}", self.clang_target()),
            "-ffreestanding".to_string(),
        ];
        if self.red_zone {
            flags.push("-mno-red-zone".to_string());
        }
        flags.extend(self.extra_cflags.iter().map(|flag| flag.to_string()));
        flags.push("-fno-stack-protector".to_string());
        flags.push("-fno-pic".to_string());
        flags
    }
}

pub static X86_64: Arch = Arch {
    name: "x86",
    triple: "x86_64-pc-minix",
    machine: "x86_64",
    red_zone: true,
    extra_cflags: &[],
};

pub static RISCV64: Arch = Arch {
    name: "riscv64",
    triple: "riscv64gc-unknown-minix",
    machine: "riscv64",
    red_zone: false,
    extra_cflags: &["-march=rv64gc", "-mabi=lp64d"],
};

pub static AARCH64: Arch = Arch {
    name: "aarch64",
    triple: "aarch64-unknown-minix",
    machine: "aarch64",
    red_zone: true,
    extra_cflags: &[],
};

pub static ALL: [&Arch; 3] = [&X86_64, &RISCV64, &AARCH64];

/// What a build that names no target gets: x86_64, which is what every caller
/// did before there was a choice.
pub static DEFAULT: &Arch = &X86_64;

/// The arch `token` names, by short name or by triple, or an error naming the
/// ones that exist.
pub fn resolve(token: &str) -> io::Result<&'static Arch> {
    ALL.iter()
        .copied()
        .find(|arch| arch.name == token || arch.triple == token)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unknown target {token:?} (expected one of {}, {}, {}, or a minix triple)",
                    X86_64.name, RISCV64.name, AARCH64.name
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_resolves_by_short_name_and_by_triple() {
        assert_eq!(resolve("x86").unwrap().triple, "x86_64-pc-minix");
        assert_eq!(resolve("riscv64gc-unknown-minix").unwrap().name, "riscv64");
        assert_eq!(resolve("aarch64").unwrap().triple, "aarch64-unknown-minix");
    }

    #[test]
    fn an_unknown_target_names_the_ones_that_exist() {
        let error = resolve("mips").unwrap_err();
        assert!(error.to_string().contains("unknown target \"mips\""));
        assert!(error.to_string().contains("riscv64"));
    }

    #[test]
    fn the_clang_target_is_the_machine_the_crt0_names() {
        assert_eq!(X86_64.clang_target(), "x86_64-unknown-none");
        assert!(X86_64.crt0(Path::new("root")).ends_with("tools/crt0-x86_64.S"));
    }

    #[test]
    fn riscv64_is_the_one_arch_that_pins_its_float_abi() {
        let flags = RISCV64.base_cflags();
        assert!(flags.iter().any(|flag| flag == "-march=rv64gc"));
        assert!(flags.iter().any(|flag| flag == "-mabi=lp64d"));
        assert!(!flags.iter().any(|flag| flag == "-mno-red-zone"));
    }

    #[test]
    fn the_red_zone_flag_is_only_where_there_is_one() {
        assert!(X86_64.base_cflags().iter().any(|f| f == "-mno-red-zone"));
        assert!(AARCH64.base_cflags().iter().any(|f| f == "-mno-red-zone"));
        assert!(!RISCV64.base_cflags().iter().any(|f| f == "-mno-red-zone"));
    }
}
