//! The diagnostic multicall: one binary, one subcommand per probe.
//!
//! Every Wayland and DRM client in this crate is a gate's probe, not an
//! application. They used to be one image each (`diag wlpanel`, `diag drmprime`,
//! and seventeen more), and every image carried all of them: thirteen separate
//! ELFs, each with a static copy of the same crate code. `diag <probe>` is the
//! same code behind one binary.
//!
//! The probe functions take `&[&str]` where `args[0]` is the program name, so
//! each one is handed `["<probe>", ...]`: the slice keeps the shape it saw when
//! it had an ELF of its own.

use crate::write_out;

/// Every probe, in the order the usage line prints them.
pub const PROBES: &[&str] = &[
    "wlclient",
    "wlkey",
    "wlx",
    "wlx2",
    "wloverlap",
    "wlmotion",
    "wlpointer",
    "wldemo",
    "wlpanel",
    "wlxd",
    "wlxe",
    "wldmabuf",
    "wlscene",
    "drminfo",
    "drmmap",
    "drmprime",
    "drmctx",
    "drmexec",
    "drmblob",
];

/// Run the probe named by `args[1]`. Returns the probe's exit code, or 2 when
/// there is no such probe.
pub fn dispatch(args: &[&str]) -> i32 {
    let Some(probe) = args.get(1).copied() else {
        return usage();
    };
    let rest = &args[1..];
    match probe {
        "wlclient" => crate::wlclient::wl_client(rest),
        "wlkey" => crate::wlclient::wl_key(rest),
        "wlx" => crate::wlclient::wl_xdg(rest),
        "wlx2" => crate::wlclient::wl_focus(rest),
        "wloverlap" => crate::wlclient::wl_overlap(rest),
        "wlmotion" => crate::wlclient::wl_motion(rest),
        "wlpointer" => crate::wlclient::wl_pointer(rest),
        "wldemo" => crate::wlclient::wl_demo(rest),
        "wlpanel" => crate::wlclient::wl_panel(rest),
        "wlxd" => crate::wlclient::wl_damage(rest),
        "wlxe" => crate::wlclient::wl_layer_popup_decoration(rest),
        "wldmabuf" => crate::wldmabuf::wldmabuf(rest),
        "wlscene" => crate::wlscene::wlscene(rest),
        "drminfo" => crate::drminfo::drminfo(rest),
        "drmmap" => crate::drmmap::drmmap(rest),
        "drmprime" => crate::drmprime::drmprime(rest),
        "drmctx" => crate::drmctx::drmctx(rest),
        "drmexec" => crate::drmexec::drmexec(rest),
        "drmblob" => crate::drmblob::drmblob(rest),
        _ => usage(),
    }
}

fn usage() -> i32 {
    write_out(b"usage: diag <probe> [args]\nprobes:");
    for probe in PROBES {
        write_out(b" ");
        write_out(probe.as_bytes());
    }
    write_out(b"\n");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_probe_is_a_usage_error() {
        assert_eq!(dispatch(&["diag"]), 2);
    }

    #[test]
    fn an_unknown_probe_is_a_usage_error() {
        assert_eq!(dispatch(&["diag", "nope"]), 2);
    }

    #[test]
    fn the_usage_list_has_no_duplicates() {
        let mut probes = PROBES.to_vec();
        probes.sort_unstable();
        probes.dedup();
        assert_eq!(probes.len(), PROBES.len());
    }
}
