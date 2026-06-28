//! ProcFS CPU information — adapted from `minix/fs/procfs/cpuinfo.{c,h}`

use crate::procfs::buf::{buf_write, buf_write_fmt};

/// x86 CPU feature flag names, indexed by `(ecx_bit, edx_bit)`.
///
/// Empty strings represent reserved or unknown bits.
static X86_FLAGS: &[&str] = &[
    "fpu", "vme", "de", "pse", "tsc", "msr", "pae", "mce", "cx8", "apic", "", "sep", "mtrr", "pge",
    "mca", "cmov", "pat", "pse36", "psn", "clfsh", "", "dts", "acpi", "mmx", "fxsr", "sse", "sse2",
    "ss", "ht", "tm", "", "pbe", "pni", "", "", "monitor", "ds_cpl", "vmx", "smx", "est", "tm2",
    "ssse3", "cid", "", "", "cx16", "xtpr", "pdcm", "", "", "dca", "sse4_1", "sse4_2", "x2apic",
    "movbe", "popcnt", "", "", "xsave", "osxsave", "", "", "", "",
];

/// CPU information structure (stub).
#[derive(Debug, Default)]
pub struct CpuInfo {
    pub vendor: u32,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub freq: u32,
    pub flags: [u32; 2],
}

/// Print names of enabled CPU feature flags.
pub fn print_cpu_flags(flags: &[u32; 2]) {
    for (i, &flag) in flags.iter().enumerate() {
        for bit in 0..32 {
            if (flag & (1 << bit)) != 0 {
                let idx = i * 32 + bit;
                if let Some(&name) = X86_FLAGS.get(idx)
                    && !name.is_empty()
                {
                    buf_write(name);
                    buf_write(" ");
                }
            }
        }
    }
    buf_write("\n");
}

/// Print a single CPU's info lines (Linux `/proc/cpuinfo` style).
pub fn print_cpu(cpu_info: &CpuInfo, id: u32) {
    buf_write_fmt(format_args!("{:<16}: {}\n", "processor", id));

    match cpu_info.vendor {
        0 => {
            buf_write_fmt(format_args!("{:<16}: {}\n", "vendor_id", "GenuineIntel"));
            buf_write_fmt(format_args!("{:<16}: {}\n", "model name", "Intel"));
        }
        1 => {
            buf_write_fmt(format_args!("{:<16}: {}\n", "vendor_id", "AuthenticAMD"));
            buf_write_fmt(format_args!("{:<16}: {}\n", "model name", "AMD"));
        }
        _ => {
            buf_write_fmt(format_args!("{:<16}: {}\n", "vendor_id", "unknown"));
            buf_write_fmt(format_args!("{:<16}: {}\n", "model name", "unknown"));
        }
    }

    buf_write_fmt(format_args!("{:<16}: {}\n", "cpu family", cpu_info.family));
    buf_write_fmt(format_args!("{:<16}: {}\n", "model", cpu_info.model));
    buf_write_fmt(format_args!("{:<16}: {}\n", "stepping", cpu_info.stepping));
    buf_write_fmt(format_args!("{:<16}: {}\n", "cpu MHz", cpu_info.freq));
    buf_write_fmt(format_args!("{:<16}: ", "flags"));
    print_cpu_flags(&cpu_info.flags);
    buf_write("\n");
}

/// Root `/proc/cpuinfo` handler.
///
/// TODO: call `sys_getmachine()` and `sys_getcpuinfo()` to obtain real
///       `machine.processors_count` and per-CPU `CpuInfo` data.
pub fn root_cpuinfo() {
    // Stub: write minimal output.
    buf_write("0\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::buf;

    #[test]
    fn print_cpu_flags_no_panic() {
        buf::buf_init(0, 256);
        let flags = [0x8000_0000, 0x0000_0000]; // PAE bit
        print_cpu_flags(&flags);
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }

    #[test]
    fn print_cpu_does_not_panic() {
        buf::buf_init(0, 512);
        let info = CpuInfo {
            vendor: 0,
            family: 6,
            model: 15,
            stepping: 2,
            freq: 2400,
            flags: [0, 0],
        };
        print_cpu(&info, 0);
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }

    #[test]
    fn root_cpuinfo_does_not_panic() {
        buf::buf_init(0, 64);
        root_cpuinfo();
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }
}
