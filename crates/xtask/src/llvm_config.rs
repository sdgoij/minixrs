//! A host-runnable `llvm-config` for the minix LLVM cross-build.
//!
//! The `llvm-config` the cross build produces is a minix ELF and cannot run on
//! the build host, but `rustc_llvm`'s build script has to run one to learn the
//! LLVM libraries, the C++ flags and the version. This reads the build tree's
//! generated `LibraryDependencies.inc` and `llvm-config.h` and answers the same
//! queries, so the build host sees the minix build's parameters.
//!
//! Usage: `cargo xtask llvm-config --build-root target/llvm-x86/build [QUERY...]`.
//! The queries are the ones `llvm-config` takes; `--cxxflags` reports the flags
//! the LLVM libraries themselves were compiled with, because `rustc_llvm`'s C++
//! shims have to be ABI-compatible with them.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::arch::{self, Arch};

struct Component {
    name: String,
    library: String,
    deps: Vec<String>,
}

/// Parse the generated `LibraryDependencies.inc`.
///
/// An entry is `{ "name", "LLVMName", true, {"dep", "dep"} },`. The dependency
/// list contains commas, so it is taken as the text between the first `{` after
/// the name field and its matching `}` rather than by comma position.
fn parse_components(build_root: &Path) -> io::Result<Vec<Component>> {
    let inc = build_root
        .join("tools")
        .join("llvm-config")
        .join("LibraryDependencies.inc");
    let text = fs::read_to_string(&inc)?;
    let mut components = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('{') else {
            continue;
        };
        let mut parts = rest.splitn(5, ',');
        let Some(name) = parts.next() else { continue };
        let name = name.trim().trim_matches('"');
        if name.is_empty() {
            continue;
        }
        let Some(library) = parts.next() else { continue };
        let library = library.trim().trim_matches('"').to_string();
        let Some(open) = rest.find('{') else { continue };
        let Some(close) = rest[open + 1..].find('}') else {
            continue;
        };
        let close = open + 1 + close;
        let deps = rest[open + 1..close]
            .split(',')
            .map(|dep| dep.trim().trim_matches('"'))
            .filter(|dep| !dep.is_empty())
            .map(String::from)
            .collect();
        components.push(Component {
            name: name.to_string(),
            library,
            deps,
        });
    }
    Ok(components)
}

/// The components in link order, consumers before their dependencies, as
/// `llvm-config` computes them.
fn compute_libs(components: &[Component], requested: &[String]) -> Vec<String> {
    fn visit(
        name: &str,
        components: &[Component],
        visited: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if !visited.insert(name.to_string()) {
            return;
        }
        if let Some(component) = components.iter().find(|component| component.name == name) {
            for dep in &component.deps {
                visit(dep, components, visited, out);
            }
        }
        out.push(name.to_string());
    }

    let mut visited = HashSet::new();
    let mut out = Vec::new();
    let wanted: Vec<String> = if requested.is_empty() {
        components
            .iter()
            .map(|component| component.name.clone())
            .collect()
    } else {
        requested.to_vec()
    };
    for name in &wanted {
        visit(name, components, &mut visited, &mut out);
    }
    out.reverse();
    out
}

fn version(build_root: &Path) -> io::Result<String> {
    let header = build_root.join("include").join("llvm").join("Config").join("llvm-config.h");
    let text = fs::read_to_string(&header).map_err(|error| {
        io::Error::new(error.kind(), format!("reading {}: {error}", header.display()))
    })?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("#define LLVM_VERSION_STRING ") {
            return Ok(rest.trim().trim_matches('"').to_string());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} has no LLVM_VERSION_STRING", header.display()),
    ))
}

/// The LLVM target the build enabled for this arch, as `--targets-built` spells it.
fn targets_built(arch: &Arch) -> &'static str {
    match arch.name {
        "x86" => "X86",
        "riscv64" => "RISCV",
        _ => "AArch64",
    }
}

fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The flags the LLVM libraries were compiled with.
///
/// `rustc_llvm` compiles its C++ shims with these, so they have to match the
/// libraries' ABI: freestanding, the port's C headers, libc++ and its `-nostdinc`
/// pair, and the LLVM source and generated include trees. The clang builtin
/// headers come from the cross build's own resource directory, which is the
/// version the libraries were built with; the host clang's is a different
/// version and can sit under a path with a space, which `rustc_llvm` would split
/// because it takes this output unquoted.
fn cxxflags(root: &Path, arch: &Arch, build_root: &Path, resource: &Path) -> String {
    let llvm = root.join("rust").join("src").join("llvm-project");
    let libcxx_build = root
        .join("target")
        .join("cxx")
        .join(format!("{}-toolchain", arch.name))
        .join("libcxx-build");
    format!(
        "-std=c++17 -fno-exceptions -fno-rtti \
         -D__STDC_CONSTANT_MACROS -D__STDC_FORMAT_MACROS -D__STDC_LIMIT_MACROS \
         -DLLVM_ON_UNIX -DHAVE_SYSEXITS_H=1 \
         -D_LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE -D_LIBCPP_NO_ABI_TAG \
         -ffreestanding -mno-red-zone -fno-stack-protector -fno-pic \
         -nostdinc -nostdinc++ \
         -isystem {}/libcxx/include -isystem {}/libcxxabi/include \
         -I{}/include/c++/v1 \
         -isystem {} \
         -I{}/tools/c-include \
         -I{}/llvm/include -I{}/include",
        slash(&llvm),
        slash(&llvm),
        slash(&libcxx_build),
        slash(resource),
        slash(root),
        slash(&llvm),
        slash(build_root),
    )
}

/// Which output flags were asked for, and which components a `--libs` query
/// names. A flag that only changes a query's formatting (`--link-static`,
/// `--quote-paths`) is dropped, and every output flag is kept so a combined
/// `--libs --system-libs` asks for both.
fn split_args(args: &[String]) -> (Vec<&str>, Vec<String>) {
    let mut queries = Vec::new();
    let mut components = Vec::new();
    for arg in args {
        if arg.starts_with("--") {
            match arg.as_str() {
                "--libs"
                | "--libnames"
                | "--ldflags"
                | "--cxxflags"
                | "--cflags"
                | "--components"
                | "--version"
                | "--help"
                | "--includedir"
                | "--libdir"
                | "--cmakedir"
                | "--bindir"
                | "--host-target"
                | "--obj-root"
                | "--src-root"
                | "--targets-built"
                | "--shared-mode"
                | "--build-mode"
                | "--assertion-mode"
                | "--has-rtti"
                | "--system-libs" => queries.push(arg.as_str()),
                _ => {}
            }
        } else {
            components.push(arg.clone());
        }
    }
    (queries, components)
}

pub fn run(
    root: &Path,
    build_root: Option<&str>,
    arch_token: &str,
    args: &[String],
) -> io::Result<bool> {
    let arch = arch::resolve(arch_token)?;
    let build_root = match build_root {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(path) => root.join(path),
        None => root
            .join("target")
            .join(format!("llvm-{}", arch.name))
            .join("build"),
    };
    let lib_dir = build_root.join("lib");
    let include_dir = build_root.join("include");
    let src_root = root.join("rust").join("src").join("llvm-project").join("llvm");

    // `llvm-config` accepts several output flags at once and prints each in
    // turn, so they are collected rather than collapsed onto one query. This
    // matters for `rustc_llvm`, which asks for `--libs --system-libs` in a
    // single call.
    let (queries, components) = split_args(args);
    let queries = if queries.is_empty() {
        vec!["--help"]
    } else {
        queries
    };

    for query in queries {
        match query {
            "--version" => println!("{}", version(&build_root)?),
            "--components" => {
                let parsed = parse_components(&build_root)?;
                for component in &parsed {
                    print!("{} ", component.name);
                }
                println!();
            }
            "--targets-built" => println!("{}", targets_built(arch)),
            "--cxxflags" => {
                let major = crate::build::llvm_minix::resource_major(&version(&build_root)?);
                let resource = build_root
                    .join("lib")
                    .join("clang")
                    .join(major)
                    .join("include");
                println!("{}", cxxflags(root, arch, &build_root, &resource));
            }
            "--cflags" => println!("-ffreestanding -mno-red-zone -fno-stack-protector -fno-pic"),
            "--ldflags" => println!("-L{}", slash(&lib_dir)),
            "--includedir" => println!("{}", slash(&include_dir)),
            "--libdir" => println!("{}", slash(&lib_dir)),
            "--cmakedir" => println!("{}/lib/cmake/llvm", slash(&build_root)),
            "--bindir" => println!("{}/bin", slash(&build_root)),
            "--host-target" => println!("{}", arch.triple),
            "--obj-root" => println!("{}", slash(&build_root)),
            "--src-root" => println!("{}", slash(&src_root)),
            "--shared-mode" => println!("static"),
            "--build-mode" => println!("Release"),
            "--assertion-mode" => println!("OFF"),
            "--has-rtti" => println!("NO"),
            "--libs" | "--libnames" => {
                let parsed = parse_components(&build_root)?;
                let requested = if components.is_empty() {
                    vec!["all".to_string()]
                } else {
                    components.clone()
                };
                for name in compute_libs(&parsed, &requested) {
                    let Some(component) = parsed.iter().find(|component| component.name == name)
                    else {
                        continue;
                    };
                    if component.library.is_empty() || component.library == "nullptr" {
                        continue;
                    }
                    if query == "--libnames" {
                        println!("lib{}.a", component.library);
                    } else {
                        println!("{}/lib{}.a", slash(&lib_dir), component.library);
                    }
                }
            }
            // The minix build is freestanding: it links no system libraries.
            "--system-libs" => {}
            // `--help` and anything unrecognized: keep the output free of
            // "quote-paths", so `rustc_llvm` uses plain, unquoted path parsing.
            _ => {
                println!("usage: cargo xtask llvm-config --build-root <dir> [QUERY...]");
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INC: &str = r#"
  } AvailableComponents[4] = {
  { "support", "LLVMSupport", true, {} },
  { "core", "LLVMCore", true, {"support", "demangle"} },
  { "demangle", "LLVMDemangle", true, {} },
  { "x86", nullptr, true, {"target", "core"} },
  { "target", "LLVMTarget", true, {"support"} },
"#;

    fn write_inc(dir: &Path) {
        let tools = dir.join("tools").join("llvm-config");
        fs::create_dir_all(&tools).unwrap();
        fs::write(tools.join("LibraryDependencies.inc"), INC).unwrap();
    }

    #[test]
    fn the_component_parser_reads_names_libraries_and_deps() {
        let dir = std::env::temp_dir().join(format!("minixrs-llvm-config-{}", std::process::id()));
        write_inc(&dir);
        let parsed = parse_components(&dir).unwrap();
        assert_eq!(parsed.len(), 5);
        assert_eq!(parsed[1].name, "core");
        assert_eq!(parsed[1].library, "LLVMCore");
        assert_eq!(parsed[1].deps, vec!["support", "demangle"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn libs_are_consumers_first_and_dependencies_follow() {
        let dir = std::env::temp_dir().join(format!("minixrs-llvm-order-{}", std::process::id()));
        write_inc(&dir);
        let parsed = parse_components(&dir).unwrap();
        let libs = compute_libs(&parsed, &["core".to_string()]);
        let core = libs.iter().position(|name| name == "core").unwrap();
        let support = libs.iter().position(|name| name == "support").unwrap();
        let demangle = libs.iter().position(|name| name == "demangle").unwrap();
        assert!(core < support && core < demangle, "{libs:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_version_comes_from_the_generated_header() {
        let dir = std::env::temp_dir().join(format!("minixrs-llvm-version-{}", std::process::id()));
        let config = dir.join("include").join("llvm").join("Config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("llvm-config.h"),
            "#define LLVM_VERSION_STRING \"23.1.1\"\n",
        )
        .unwrap();
        assert_eq!(version(&dir).unwrap(), "23.1.1");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_version_header_is_an_error_not_unknown() {
        let dir = std::env::temp_dir().join(format!(
            "minixrs-llvm-no-version-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(version(&dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_cxxflags_name_the_minix_headers_and_the_llvm_trees() {
        let root = Path::new("root");
        let arch = arch::resolve("x86").unwrap();
        let flags = cxxflags(
            root,
            arch,
            Path::new("root/target/llvm-x86/build"),
            Path::new("root/target/llvm-x86/build/lib/clang/23/include"),
        );
        assert!(flags.contains("rust/src/llvm-project/libcxx/include"));
        assert!(flags.contains("target/cxx/x86-toolchain/libcxx-build/include/c++/v1"));
        assert!(flags.contains("target/llvm-x86/build/include"));
        assert!(flags.contains("-isystem root/target/llvm-x86/build/lib/clang/23/include"));
        assert!(flags.contains("-fno-exceptions"));
        assert!(flags.contains("-nostdinc++"));
    }

    #[test]
    fn a_libs_query_is_kept_when_system_libs_is_also_asked_for() {
        let args: Vec<String> = ["--link-static", "--libs", "--system-libs", "core"]
            .iter()
            .map(|word| word.to_string())
            .collect();
        let (queries, components) = split_args(&args);
        assert_eq!(queries, vec!["--libs", "--system-libs"]);
        assert_eq!(components, vec!["core"]);
    }
}
