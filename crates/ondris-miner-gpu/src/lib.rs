//! GPU miner for Ondris. See `blake3_ref` for why there's a from-scratch
//! BLAKE3 reimplementation in here alongside the real `blake3` crate that
//! the rest of the project uses.

pub mod blake3_ref;
pub mod gpu;
pub mod ondris_hash_ref;
