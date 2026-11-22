//! Build the LLVM 23 tablegen programs the *build host* runs.
//!
//! A cross build of clang needs `clang-tblgen` and `llvm-min-tblgen` in addition
//! to `llvm-tblgen`, and it runs all three on the build host. The fork's LLVM
//! build tree carries only `llvm-tblgen`, so this builds the three from
//! `rust/src/llvm-project` with the host's own compiler.
//!
//! A tablegen from a different LLVM is not interchangeable with the sources: the
//! `.td` files use records and emitters an older tool does not know. The
//! version check at the end is therefore part of the command, not a note.
//!
//! Output: `target/llvm/host/{Release/,}bin` (`Release/` with the Visual Studio
//! generator, which is the one the Windows host uses). Point a cross build at
//! that directory with `-DLLVM_NATIVE_TOOL_DIR`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cc;
use crate::toolchain;

/// The host tools a cross build of clang runs, in the order MSBuild resolves
/// them (each target also carries its own dependencies).
const TOOLS: [&str; 3] = ["llvm-tblgen", "clang-tblgen", "llvm-min-tblgen"];

fn cmake_args(root: &Path, build: &Path) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if cfg!(windows) {
        args.extend(["-G", "Visual Studio 17 2022", "-A", "x64"].map(String::from));
    } else {
        args.extend(["-G", "Ninja"].map(String::from));
    }
    args.push("-S".to_string());
    args.push(root.join("rust/src/llvm-project/llvm").display().to_string());
    args.push("-B".to_string());
    args.push(build.display().to_string());
    for option in [
        "-DCMAKE_BUILD_TYPE=Release",
        "-DLLVM_ENABLE_PROJECTS=clang",
        "-DLLVM_TARGETS_TO_BUILD=Native",
        "-DLLVM_INCLUDE_TESTS=OFF",
        "-DLLVM_INCLUDE_BENCHMARKS=OFF",
        "-DLLVM_INCLUDE_EXAMPLES=OFF",
        "-DLLVM_INCLUDE_DOCS=OFF",
        "-DLLVM_ENABLE_ZLIB=OFF",
        "-DLLVM_ENABLE_ZSTD=OFF",
        "-DLLVM_ENABLE_LIBXML2=OFF",
        "-DLLVM_ENABLE_LIBEDIT=OFF",
        "-DCLANG_ENABLE_STATIC_ANALYZER=OFF",
    ] {
        args.push(option.to_string());
    }
    args
}

/// Where the Visual Studio generator and Ninja each put the executables.
pub fn bin_dir(root: &Path) -> PathBuf {
    let build = root.join("target").join("llvm").join("host");
    if cfg!(windows) {
        build.join("Release").join("bin")
    } else {
        build.join("bin")
    }
}

pub fn run(root: &Path) -> io::Result<()> {
    let cmake = need("cmake")?;
    if !cfg!(windows) {
        // Ninja is the generator there, and cmake only names it when it is absent.
        need("ninja")?;
    }
    let version = source_version(root)?;
    let build = root.join("target").join("llvm").join("host");
    fs::create_dir_all(&build)?;

    let mut configure = Command::new(&cmake);
    configure.args(cmake_args(root, &build));
    step(&mut configure, "configuring the host tablegen build")?;

    let mut build_command = Command::new(&cmake);
    build_command.arg("--build").arg(&build);
    if cfg!(windows) {
        build_command.args(["--config", "Release"]);
    }
    for tool in TOOLS {
        build_command.arg("--target").arg(tool);
    }
    build_command.args(["--parallel", "16"]);
    step(&mut build_command, "building the host tablegen tools")?;

    let bin = bin_dir(root);
    for tool in TOOLS {
        let path = bin.join(toolchain::exe(tool));
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} was not produced", path.display()),
            ));
        }
        check_version(&path, &version)?;
        println!("wrote {} ({version})", path.display());
    }
    Ok(())
}

/// The version the checked-in LLVM sources carry, from the same file LLVM's own
/// CMake reads.
pub fn source_version(root: &Path) -> io::Result<String> {
    let path = root
        .join("rust/src/llvm-project/cmake/Modules/LLVMVersion.cmake");
    let text = fs::read_to_string(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("reading {}: {error}", path.display()),
        )
    })?;
    parse_version(&text).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} names no LLVM_VERSION_MAJOR/MINOR/PATCH", path.display()),
        )
    })
}

fn parse_version(text: &str) -> Option<String> {
    let field = |name: &str| -> Option<String> {
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("set(LLVM_VERSION_{name}")))?;
        let value = line.split_whitespace().nth(1)?;
        Some(value.trim_end_matches(')').to_string())
    };
    Some(format!(
        "{}.{}.{}",
        field("MAJOR")?,
        field("MINOR")?,
        field("PATCH")?
    ))
}

fn check_version(tool: &Path, expected: &str) -> io::Result<()> {
    let output = Command::new(tool).arg("--version").output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && text.contains(expected) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} does not report LLVM {expected}: {}",
            tool.display(),
            text.trim()
        )))
    }
}

fn need(name: &str) -> io::Result<PathBuf> {
    toolchain::find_on_path(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("`{name}` not found on PATH (needed by the LLVM host-tool build)"),
        )
    })
}

fn step(command: &mut Command, what: &str) -> io::Result<()> {
    if cc::run(command)? {
        Ok(())
    } else {
        Err(io::Error::other(format!("{what} failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configure_names_the_source_the_build_and_clang() {
        let args = cmake_args(Path::new("repo"), Path::new("out"));
        let source = Path::new("repo")
            .join("rust/src/llvm-project/llvm")
            .display()
            .to_string();
        assert!(args.contains(&source));
        assert!(args.contains(&"-DLLVM_ENABLE_PROJECTS=clang".to_string()));
        assert!(args.contains(&"-DLLVM_TARGETS_TO_BUILD=Native".to_string()));
        assert!(args.contains(&"-DLLVM_INCLUDE_TESTS=OFF".to_string()));
    }

    #[test]
    fn every_tool_a_cross_build_runs_is_built() {
        assert_eq!(TOOLS, ["llvm-tblgen", "clang-tblgen", "llvm-min-tblgen"]);
    }

    #[test]
    fn the_source_version_is_read_from_the_file_llvm_reads() {
        let text = "\
if(NOT DEFINED LLVM_VERSION_MAJOR)
  set(LLVM_VERSION_MAJOR 23)
endif()
if(NOT DEFINED LLVM_VERSION_MINOR)
  set(LLVM_VERSION_MINOR 1)
endif()
if(NOT DEFINED LLVM_VERSION_PATCH)
  set(LLVM_VERSION_PATCH 1)
endif()
";
        assert_eq!(parse_version(text).as_deref(), Some("23.1.1"));
        assert_eq!(parse_version("nothing here"), None);
    }

    #[test]
    fn the_bin_directory_matches_the_generator() {
        let bin = bin_dir(Path::new("repo"));
        let text = bin.display().to_string().replace('\\', "/");
        assert!(text.ends_with("target/llvm/host/Release/bin") || text.ends_with("target/llvm/host/bin"));
    }
}
