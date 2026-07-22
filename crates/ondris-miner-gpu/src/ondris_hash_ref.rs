//! Full OndrisHash algorithm, built only from this crate's own
//! `blake3_ref` — no dependency on the real `blake3` crate. This is the
//! last Rust-side checkpoint before transcribing to OpenCL: if this
//! matches `ondris_pow::ondris_hash_with_accesses` bit for bit, then
//! `blake3_ref` plus this FNV mixing logic together reproduce the real
//! algorithm exactly, and the OpenCL kernel only needs to be a faithful,
//! mechanical translation of what's here.

use crate::blake3_ref;

const ITEM_SIZE: usize = 128;
const FNV_PRIME: u32 = 0x0100_0193;

fn fnv(a: u32, b: u32) -> u32 {
    a.wrapping_mul(FNV_PRIME) ^ b
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// `dataset` is the raw dataset bytes (see `ondris_pow::Dataset::bytes`).
pub fn ondris_hash_with_accesses(
    header_bytes: &[u8],
    nonce: u64,
    dataset: &[u8],
    accesses: usize,
) -> [u8; 32] {
    let mut input = Vec::with_capacity(header_bytes.len() + 8);
    input.extend_from_slice(header_bytes);
    input.extend_from_slice(&nonce.to_le_bytes());
    let seed = blake3_ref::hash(&input);

    let mut mix = blake3_ref::xof_from_32_bytes(&seed, ITEM_SIZE);

    let seed_word0 = read_u32_le(&seed, 0);
    let words_per_item = ITEM_SIZE / 4;
    let n_items = (dataset.len() / ITEM_SIZE).max(1) as u64;

    for i in 0..accesses {
        let mix_word = read_u32_le(&mix, (i % words_per_item) * 4);
        let p = (fnv(seed_word0 ^ i as u32, mix_word) as u64 % n_items) as usize;
        let item = &dataset[p * ITEM_SIZE..p * ITEM_SIZE + ITEM_SIZE];
        for w in 0..words_per_item {
            let mixed = fnv(read_u32_le(&mix, w * 4), read_u32_le(item, w * 4));
            write_u32_le(&mut mix, w * 4, mixed);
        }
    }

    let mut compressed = [0u8; 32];
    for i in 0..8 {
        let base = i * 16;
        let w0 = read_u32_le(&mix, base);
        let w1 = read_u32_le(&mix, base + 4);
        let w2 = read_u32_le(&mix, base + 8);
        let w3 = read_u32_le(&mix, base + 12);
        write_u32_le(&mut compressed, i * 4, fnv(fnv(fnv(w0, w1), w2), w3));
    }

    let mut final_input = Vec::with_capacity(64);
    final_input.extend_from_slice(&seed);
    final_input.extend_from_slice(&compressed);
    blake3_ref::hash(&final_input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ondris_primitives::Hash256;

    #[test]
    fn matches_the_real_ondris_pow_crate() {
        let seed = Hash256::hash(b"gpu-validation-seed-v2");
        let dataset = ondris_pow::Dataset::generate_with_sizes(0, seed, 4096, 8192);

        for header in [&b"header-a"[..], b"a different, longer header value"] {
            for nonce in [0u64, 1, 42, u64::MAX, 123456789] {
                let expected = ondris_pow::ondris_hash_with_accesses(header, nonce, &dataset, 8);
                let got = ondris_hash_with_accesses(header, nonce, dataset.bytes(), 8);
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
        let seed = Hash256::hash(b"default-size-check-v2");
        let dataset = ondris_pow::Dataset::generate_with_sizes(
            0,
            seed,
            ondris_pow::CACHE_SIZE,
            ondris_pow::DATASET_SIZE,
        );
        let header = b"a realistic-length header field for this check";
        let expected = ondris_pow::ondris_hash(header, 7, &dataset);
        let got = ondris_hash_with_accesses(header, 7, dataset.bytes(), ondris_pow::ACCESSES);
        assert_eq!(got, *expected.as_bytes());
    }
}
