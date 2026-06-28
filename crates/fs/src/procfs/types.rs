//! ProcFS core types — adapted from `minix/fs/procfs/type.h`

/// Load average sample.
#[derive(Debug, Clone, Copy, Default)]
pub struct Load {
    /// Number of ticks this sample covers.
    pub ticks: u64,
    /// CPU load during that interval.
    pub proc_load: i64,
}

/// Describes a single file or directory in the ProcFS tree.
///
/// Static files use `FileData::Static(fn())` — the handler takes no arguments.
/// Dynamic (per-PID) files use `FileData::Dynamic(fn(i32))` — the handler
/// receives the kernel slot number.
pub struct File {
    pub name: &'static str,
    pub mode: u32,
    pub data: FileData,
}

/// The custom data associated with a file entry.
pub enum FileData {
    /// No data (sentinel terminator).
    None,
    /// Static file handler: no slot argument.
    Static(fn()),
    /// Dynamic (per-PID) file handler: receives a slot number.
    Dynamic(fn(i32)),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_default_is_zero() {
        let l = Load::default();
        assert_eq!(l.ticks, 0);
        assert_eq!(l.proc_load, 0);
    }

    #[test]
    fn file_data_variants() {
        static F: fn() = || {};
        static D: fn(i32) = |_: i32| {};

        match FileData::None {
            FileData::None => {}
            _ => panic!("expected None"),
        }
        match FileData::Static(F) {
            FileData::Static(_) => {}
            _ => panic!("expected Static"),
        }
        match FileData::Dynamic(D) {
            FileData::Dynamic(_) => {}
            _ => panic!("expected Dynamic"),
        }
    }
}
