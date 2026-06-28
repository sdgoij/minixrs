//! Link, unlink, rename, readlink — adapted from `minix/fs/mfs/link.c`

pub fn fs_link() -> i32 {
    todo!("fs_link: not yet wired")
}
pub fn fs_unlink() -> i32 {
    todo!("fs_unlink: not yet wired")
}
pub fn fs_rdlink() -> i32 {
    todo!("fs_rdlink: not yet wired")
}
pub fn fs_rename() -> i32 {
    todo!("fs_rename: not yet wired")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_link_panics() {
        fs_link();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_unlink_panics() {
        fs_unlink();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_rdlink_panics() {
        fs_rdlink();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_rename_panics() {
        fs_rename();
    }
}
