//! Libraries: libc, libm, libutil re-implementation.

#![no_std]

extern crate alloc;

pub mod libminixfs;
pub mod vtreefs;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        // Placeholder — real tests are in libminixfs.
        let _x = 1 + 1;
    }
}
