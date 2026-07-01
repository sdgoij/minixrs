#![no_std]
#![no_main]

#[unsafe(no_mangle)]
pub fn main(argc: i32, argv: *const *const u8) -> i32 {
    let mut buf = [""; 64];
    let args = userland::parse_args(argc, argv, &mut buf);
    userland::init(args)
}
