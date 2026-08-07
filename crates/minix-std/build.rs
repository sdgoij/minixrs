//! Emit the `minix_userspace` cfg for actual Minix userland builds.
//!
//! The OS itself builds against the JSON targets (`"os": "none"`), while the
//! in-tree rustc targets (`x86_64-pc-minix` etc.) use `"os": "minix"`. Both
//! run the same userspace ABI, so the crate's real implementations are gated
//! on `minix_userspace` instead of `target_os = "none"` directly.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(minix_userspace)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" || target_os == "minix" {
        println!("cargo:rustc-cfg=minix_userspace");
    }
}
