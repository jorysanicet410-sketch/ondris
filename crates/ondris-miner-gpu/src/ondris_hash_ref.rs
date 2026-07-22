//! Full OndrisHash mixing algorithm, built only from this crate's own
//! `blake3_ref` and `xoshiro_ref` — no dependency on the real `blake3` /
//! `rand_xoshiro` crates. This is the last Rust-side checkpoint before
//! transcribing to OpenCL: if this matches `ondris_pow::ondris_hash_with_sizes`
//! bit for bit, then `blake3_ref` + `xoshiro_ref` + this mixing logic
//! together reproduce the real algorithm exactly, and the OpenCL kernel
//! only needs to be a faithful, mechanical translation of what's here.

use crate::blake3_ref;
use crate::xoshiro_ref::Xoshiro256StarStar;

const ITEM_SIZE: usize = 32;

/// `dataset` is the raw dataset bytes (see `ondris_pow::Dataset::bytes`).
pub fn ondris_hash_with_sizes(
    header_bytes: &[u8],
    nonce: u64,
    dataset: &[u8],
    scratchpad_size: usize,
    mix_rounds: usize,
) -> [u8; 32] {
    let mut input = Vec::with_capacity(header_bytes.len() + 8);
    input.extend_from_slice(header_bytes);
    input.extend_from_slice(&nonce.to_le_bytes());
    let seed = blake3_ref::hash(&input);

    let mut rng = Xoshiro256StarStar::from_seed(seed);
    let n_blocks = (scratchpad_size / ITEM_SIZE).max(1);
    let n_items = (dataset.len() / ITEM_SIZE).max(1) as u64;
    let mut scratchpad = vec![0u8; n_blocks * ITEM_SIZE];

    for b in 0..n_blocks {
        let idx = (rng.next_u64() % n_items) as usize;
        let off = b * ITEM_SIZE;
        let item = &dataset[idx * ITEM_SIZE..idx * ITEM_SIZE + ITEM_SIZE];
        for k in 0..ITEM_SIZE {
            scratchpad[off + k] = item[k] ^ seed[k];
        }
    }

    for _round in 0..mix_rounds {
        for b in 0..n_blocks {
            let dep_idx = (rng.next_u64() as usize) % n_blocks;
            let off = b * ITEM_SIZE;
            let dep_off = dep_idx * ITEM_SIZE;

            let mut buf = [0u8; ITEM_SIZE * 2];
            buf[..ITEM_SIZE].copy_from_slice(&scratchpad[off..off + ITEM_SIZE]);
            buf[ITEM_SIZE..].copy_from_slice(&scratchpad[dep_off..dep_off + ITEM_SIZE]);
            let out = blake3_ref::hash(&buf);

            scratchpad[off..off + ITEM_SIZE].copy_from_slice(&out);
        }
    }

    blake3_ref::hash(&scratchpad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ondris_primitives::Hash256;

    #[test]
    fn matches_the_real_ondris_pow_crate() {
        let seed = Hash256::hash(b"gpu-validation-seed");
        let dataset = ondris_pow::Dataset::generate_with_sizes(0, seed, 4096, 8192);

        for header in [&b"header-a"[..], b"a different, longer header value"] {
            for nonce in [0u64, 1, 42, u64::MAX, 123456789] {
                let expected = ondris_pow::ondris_hash_with_sizes(header, nonce, &dataset, 4096, 3);
                let got = ondris_hash_with_sizes(header, nonce, dataset.bytes(), 4096, 3);
                assert_eq!(
                    got,
                    *expected.as_bytes(),
                    "mismatch for header={header:?} nonce={nonce}"
                );
            }
        }
    }

    #[test]
    fn matches_the_real_crate_at_default_sizes() {
        // The real default sizes (SCRATCHPAD_SIZE etc.) — slower, but this
        // is exactly what the GPU kernel needs to reproduce, including
        // the multi-chunk BLAKE3 path over a multi-megabyte scratchpad.
        let seed = Hash256::hash(b"default-size-check");
        let dataset = ondris_pow::Dataset::generate_with_sizes(
            0,
            seed,
            ondris_pow::CACHE_SIZE,
            ondris_pow::DATASET_SIZE,
        );
        let header = b"a realistic-length header field for this check";
        let expected = ondris_pow::ondris_hash(header, 7, &dataset);
        let got = ondris_hash_with_sizes(
            header,
            7,
            dataset.bytes(),
            ondris_pow::SCRATCHPAD_SIZE,
            ondris_pow::MIX_ROUNDS,
        );
        assert_eq!(got, *expected.as_bytes());
    }
}
