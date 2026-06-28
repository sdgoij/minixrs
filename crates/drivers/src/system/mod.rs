//! System drivers: GPIO, Kernel Log, Random Number Generator
//!
//! These are the simplest drivers, ported from
//! `.refs/minix-3.3.0/minix/drivers/system/`

pub mod gpio;
pub mod klog;
pub mod random;
