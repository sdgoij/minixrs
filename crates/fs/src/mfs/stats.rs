//! Filesystem statistics — adapted from `minix/fs/mfs/stats.c`

use crate::mfs::types::*;

// Reference: stats.c count_free_bits()
pub fn count_free_bits(_sp: &SuperBlock, _map: i32) -> u32 {
    todo!("count_free_bits: bitmap iteration not yet wired");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_count_free_bits_panics() {
        let sp = SuperBlock::default();
        count_free_bits(&sp, 0);
    }
}
