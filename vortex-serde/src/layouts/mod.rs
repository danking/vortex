mod read;
mod write;

#[cfg(test)]
mod tests;

pub const MAGIC_BYTES: [u8; 4] = *b"VRX1";

pub use read::*;
pub use write::*;
