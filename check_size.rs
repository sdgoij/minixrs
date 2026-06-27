
pub union Foo {
    pub a: [u8; 48],
    pub b: i64,
}
pub struct Bar {
    pub x: i32,
    pub y: i32,
    pub z: Foo,
}
fn main() {
    println!("Payload size: {}", std::mem::size_of::<Foo>());
    println!("Message size: {}", std::mem::size_of::<Bar>());
}
