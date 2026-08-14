//! Native `llvm-config` emulator for the minix LLVM cross-build.
//!
//! The real `llvm-config` produced by the cross build is an
//! `x86_64-unknown-none` binary that cannot run on the Windows build host.
//! rustc's bootstrap and `rustc_llvm`'s build script need a *runnable*
//! llvm-config that reports the cross build's parameters, so this program
//! reads the build tree's generated data (`tools/llvm-config/*.inc`) and
//! answers the queries `rustc_llvm/build.rs` makes (--version, --components,
//! --cxxflags, --ldflags, --libs).
//!
//! Build: `rustc tools/llvm-config-minix.rs -O -o target/cxx/llvm-config.exe`
//! (must sit next to `llvm-build/` so the build tree is locatable).
//!
//! The reported `--cxxflags` match how the LLVM libraries themselves were
//! compiled (the flags `rustc_llvm` feeds to its C++ shims must produce
//! ABI-compatible objects): the freestanding minix profile plus the libc++
//! header set and `cxx-nothreads.h`.

use std::env;
use std::path::Path;

struct Component {
    name: String,
    library: String,
    deps: Vec<String>,
}

fn parse_components(build_root: &Path) -> Vec<Component> {
    let inc_path = build_root
        .join("tools")
        .join("llvm-config")
        .join("LibraryDependencies.inc");
    let text = std::fs::read_to_string(&inc_path).expect("read LibraryDependencies.inc");
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Entry: { "name", "LLVMX", true, {"dep", "dep"} },
        if !line.starts_with('{') {
            continue;
        }
        let Some(rest) = line.strip_prefix('{') else {
            continue;
        };
        let mut parts = rest.splitn(5, ',');
        let Some(name_q) = parts.next() else { continue };
        let name = name_q.trim().trim_matches('"');
        if name.is_empty() {
            continue;
        }
        let Some(lib_q) = parts.next() else { continue };
        let library = lib_q.trim().trim_matches('"').to_string();
        // The deps live in the braced list after the `true`/`false` field;
        // take the region between `{` and its matching `}` instead of relying
        // on comma positions (the list itself contains commas).
        let Some(open) = rest.find('{') else {
            continue;
        };
        let Some(close) = rest[open + 1..].find('}') else {
            continue;
        };
        let close = open + 1 + close;
        let deps = rest[open + 1..close]
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        out.push(Component {
            name: name.to_string(),
            library,
            deps,
        });
    }
    out
}

fn compute_libs(comps: &[Component], requested: &[String]) -> Vec<String> {
    // visit() pushes leaf-first; reverse for consumers-first link order
    // (matching llvm-config's computeLibsForComponents).
    fn visit(
        name: &str,
        comps: &[Component],
        visited: &mut std::collections::HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if !visited.insert(name.to_string()) {
            return;
        }
        if let Some(c) = comps.iter().find(|c| c.name == name) {
            for dep in &c.deps {
                visit(dep, comps, visited, out);
            }
        }
        out.push(name.to_string());
    }
    let mut visited = std::collections::HashSet::new();
    let mut out = Vec::new();
    let wanted: Vec<String> = if requested.is_empty() {
        comps.iter().map(|c| c.name.clone()).collect()
    } else {
        requested.to_vec()
    };
    for w in &wanted {
        visit(w, comps, &mut visited, &mut out);
    }
    out.reverse();
    out
}

fn main() {
    // Locate the build tree: this exe lives at <root>/target/cxx/llvm-config.exe,
    // the LLVM build at <root>/target/cxx/llvm-build.
    let exe = env::current_exe().expect("current_exe");
    let build_root = exe.parent().expect("exe parent").join("llvm-build");
    let lib_dir = build_root.join("lib");
    let inc_dir = build_root.join("include");
    let cxx_nothreads = build_root.join("..").join("cxx-nothreads.h");

    let args: Vec<String> = env::args().skip(1).collect();
    let mut query = String::new();
    let mut components: Vec<String> = Vec::new();
    for a in &args {
        match a.as_str() {
            "--link-static" | "--link-shared" | "--system-libs" | "--quote-paths" => {}
            "--libs" | "--ldflags" | "--cxxflags" | "--components" | "--version" | "--help"
            | "--includedir" | "--libdir" | "--cmakedir" | "--bindir" | "--host-target"
            | "--obj-root" | "--src-root" => query = a.clone(),
            _ => components.push(a.clone()),
        }
    }

    let src_root = "C:/Users/T/src/github.com/sdgoij/minixrs/rust/src/llvm-project/llvm";

    match query.as_str() {
        "--version" => println!("22.1.8"),
        "--components" => {
            let comps = parse_components(&build_root);
            for c in &comps {
                print!("{} ", c.name);
            }
            println!();
        }
        "--cxxflags" => {
            // Same profile the LLVM libraries were built with, so the
            // rustc_llvm C++ shims link ABI-compatibly. (-mno-red-zone and
            // -ffreestanding are dropped by rustc_llvm's cross filter anyway.)
            let llvm_root = "C:/Users/T/src/github.com/sdgoij/minixrs/rust/src/llvm-project";
            let flags = format!(
                "-std=c++17 -fno-exceptions -fno-rtti \
                 -D__STDC_CONSTANT_MACROS -D__STDC_FORMAT_MACROS -D__STDC_LIMIT_MACROS \
                 -DLLVM_ON_UNIX -DHAVE_SYSEXITS_H=1 \
                 -include {} \
                 -I{llvm_root}/libcxx/include -I{}/tools/c-include \
                 -I{llvm_root}/libcxxabi/include -I{}/libcxx-build/include/c++/v1 \
                 -I{src_root}/include -I{}",
                cxx_nothreads.display(),
                "C:/Users/T/src/github.com/sdgoij/minixrs",
                "C:/Users/T/src/github.com/sdgoij/minixrs/target/cxx",
                inc_dir.display(),
            );
            println!("{flags}");
        }
        "--ldflags" => println!("-L{}", lib_dir.display()),
        "--includedir" => println!("{}", inc_dir.display()),
        "--libdir" => println!("{}", lib_dir.display()),
        "--cmakedir" => println!("{}/lib/cmake/llvm", build_root.display()),
        "--bindir" => println!("{}/bin", build_root.display()),
        "--host-target" => println!("x86_64-pc-minix"),
        "--obj-root" => println!("{}", build_root.display()),
        "--src-root" => println!("{src_root}"),
        "--libs" => {
            let comps = parse_components(&build_root);
            // No components -> "all" (like the real llvm-config).
            let requested = if components.is_empty() {
                vec!["all".to_string()]
            } else {
                components.clone()
            };
            for lib in compute_libs(&comps, &requested) {
                if let Some(c) = comps.iter().find(|c| c.name == lib) {
                    // Group components ("all", "x86", ...) have no library
                    // of their own — their deps are listed separately.
                    if c.library.is_empty() || c.library == "nullptr" {
                        continue;
                    }
                    println!("{}/lib{}.a", lib_dir.display(), c.library);
                }
            }
        }
        // --help: keep it free of "quote-paths" so rustc_llvm uses plain
        // (unquoted) path parsing.
        _ => {}
    }
}
