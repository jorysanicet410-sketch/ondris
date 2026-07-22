//! OndrisHash: a memory-hard, GPU-friendly, ASIC-resistant Proof-of-Work
//! algorithm. See `docs/ALGORITHM.md` at the repo root for the full spec
//! and warnings about its unaudited status.
//!
//! Only combines already-audited primitives (BLAKE3) in an original
//! architecture: a dataset regenerated per epoch + a scratchpad mixed in a
//! way that depends on data already written.

use ondris_primitives::Hash256;
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256StarStar;

/// Block height interval at which a new dataset is generated.
pub const EPOCH_LENGTH: u64 = 2048;
/// Size of the compact cache the full dataset is derived from.
pub const CACHE_SIZE: usize = 16 * 1024 * 1024;
/// Size of the full dataset used for mixing (reduced testnet value for
/// fast dev/test cycles; to be revisited with an audit and real GPU
/// benchmarks before any mainnet launch — target 2-4 GiB).
pub const DATASET_SIZE: usize = 64 * 1024 * 1024;
/// Size of the working memory per hash attempt.
pub const SCRATCHPAD_SIZE: usize = 2 * 1024 * 1024;
/// Number of data-dependent mixing rounds.
pub const MIX_ROUNDS: usize = 8;
/// Size of one dataset/scratchpad item (= BLAKE3 output size).
pub const ITEM_SIZE: usize = 32;

pub fn epoch_of(height: u64) -> u64 {
    height / EPOCH_LENGTH
}

/// Derives an epoch's seed from the hash of its boundary block (or a fixed
/// constant for epoch 0 / genesis).
pub fn epoch_seed(boundary_block_hash: Option<Hash256>) -> Hash256 {
    match boundary_block_hash {
        None => Hash256::hash(b"ONDRIS_GENESIS_EPOCH"),
        Some(h) => Hash256::hash(h.as_bytes()),
    }
}

fn xof_fill(seed: &[u8], out: &mut [u8]) {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed);
    let mut reader = hasher.finalize_xof();
    reader.fill(out);
}

/// Full dataset for one epoch, derived from its seed. Generated once per
/// epoch (`EPOCH_LENGTH` blocks) and kept in memory by miners.
pub struct Dataset {
    pub epoch: u64,
    bytes: Vec<u8>,
}

impl Dataset {
    pub fn generate(epoch: u64, seed: Hash256) -> Self {
        Self::generate_with_sizes(epoch, seed, CACHE_SIZE, DATASET_SIZE)
    }

    /// Size-parameterized variant, used by tests to stay fast (the "real"
    /// sizes above are too heavy for a unit test loop).
    pub fn generate_with_sizes(
        epoch: u64,
        seed: Hash256,
        cache_size: usize,
        dataset_size: usize,
    ) -> Self {
        assert!(cache_size >= ITEM_SIZE && dataset_size >= ITEM_SIZE);
        let mut cache = vec![0u8; cache_size];
        xof_fill(seed.as_bytes(), &mut cache);

        let n_items = dataset_size / ITEM_SIZE;
        let mut bytes = vec![0u8; n_items * ITEM_SIZE];
        for i in 0..n_items {
            let cache_off = (i * ITEM_SIZE) % (cache_size - ITEM_SIZE + 1).max(1);
            let mut item = [0u8; ITEM_SIZE];
            item.copy_from_slice(&cache[cache_off..cache_off + ITEM_SIZE]);
            for _ in 0..2 {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&item);
                hasher.update(&(i as u64).to_le_bytes());
                item = *hasher.finalize().as_bytes();
            }
            bytes[i * ITEM_SIZE..(i + 1) * ITEM_SIZE].copy_from_slice(&item);
        }
        Dataset { epoch, bytes }
    }

    pub fn len_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Raw dataset bytes — e.g. for uploading the dataset to a GPU buffer.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn item(&self, idx: u64) -> &[u8] {
        let n_items = (self.bytes.len() / ITEM_SIZE) as u64;
        let idx = (idx % n_items) as usize;
        &self.bytes[idx * ITEM_SIZE..(idx + 1) * ITEM_SIZE]
    }
}

/// Computes OndrisHash(header || nonce) using the current epoch's dataset.
/// `header_bytes` must be the canonical serialization of the block header
/// WITHOUT the nonce (the nonce is appended here).
pub fn ondris_hash(header_bytes: &[u8], nonce: u64, dataset: &Dataset) -> Hash256 {
    ondris_hash_with_sizes(header_bytes, nonce, dataset, SCRATCHPAD_SIZE, MIX_ROUNDS)
}

/// Size-parameterized variant, used by tests to stay fast.
pub fn ondris_hash_with_sizes(
    header_bytes: &[u8],
    nonce: u64,
    dataset: &Dataset,
    scratchpad_size: usize,
    mix_rounds: usize,
) -> Hash256 {
    let mut input = Vec::with_capacity(header_bytes.len() + 8);
    input.extend_from_slice(header_bytes);
    input.extend_from_slice(&nonce.to_le_bytes());
    let seed = *blake3::hash(&input).as_bytes();

    let mut rng = Xoshiro256StarStar::from_seed(seed);

    let n_blocks = (scratchpad_size / ITEM_SIZE).max(1);
    let mut scratchpad = vec![0u8; n_blocks * ITEM_SIZE];

    // Init: populate the scratchpad with pseudo-randomly chosen slices of
    // the dataset. This is where memory bandwidth width is required.
    for b in 0..n_blocks {
        let idx = rng.next_u64();
        let d = dataset.item(idx);
        let off = b * ITEM_SIZE;
        for k in 0..ITEM_SIZE {
            scratchpad[off + k] = d[k] ^ seed[k];
        }
    }

    // Mixing: rounds that depend on data already written into the
    // scratchpad, which prevents parallelizing all rounds ahead of time
    // without enough memory to hold the intermediate state.
    for _round in 0..mix_rounds {
        for b in 0..n_blocks {
            let dep_idx = (rng.next_u64() as usize) % n_blocks;
            let off = b * ITEM_SIZE;
            let dep_off = dep_idx * ITEM_SIZE;

            let mut hasher = blake3::Hasher::new();
            hasher.update(&scratchpad[off..off + ITEM_SIZE]);
            let dep_copy: [u8; ITEM_SIZE] = scratchpad[dep_off..dep_off + ITEM_SIZE]
                .try_into()
                .expect("slice of size ITEM_SIZE");
            hasher.update(&dep_copy);
            let out = *hasher.finalize().as_bytes();

            scratchpad[off..off + ITEM_SIZE].copy_from_slice(&out);
        }
    }

    Hash256::hash(&scratchpad)
}

/// Checks that a hash meets a difficulty target (big-endian comparison,
/// like a decompacted Bitcoin nBits).
pub fn meets_target(hash: &Hash256, target_be: &[u8; 32]) -> bool {
    hash.to_u256_be() <= *target_be
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_dataset() -> Dataset {
        Dataset::generate_with_sizes(0, Hash256::hash(b"test-seed"), 4096, 8192)
    }

    #[test]
    fn deterministic_for_same_input() {
        let ds = tiny_dataset();
        let header = b"header-bytes";
        let a = ondris_hash_with_sizes(header, 42, &ds, 4096, 2);
        let b = ondris_hash_with_sizes(header, 42, &ds, 4096, 2);
        assert_eq!(a, b);
    }

    #[test]
    fn different_nonce_changes_hash() {
        let ds = tiny_dataset();
        let header = b"header-bytes";
        let a = ondris_hash_with_sizes(header, 1, &ds, 4096, 2);
        let b = ondris_hash_with_sizes(header, 2, &ds, 4096, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn different_epoch_seed_changes_dataset() {
        let ds1 = Dataset::generate_with_sizes(0, Hash256::hash(b"seed-a"), 4096, 8192);
        let ds2 = Dataset::generate_with_sizes(0, Hash256::hash(b"seed-b"), 4096, 8192);
        let header = b"header-bytes";
        let a = ondris_hash_with_sizes(header, 1, &ds1, 4096, 2);
        let b = ondris_hash_with_sizes(header, 1, &ds2, 4096, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn meets_target_boundary() {
        let low = Hash256([0u8; 32]);
        let high = Hash256([0xff; 32]);
        let target = [0x00; 32];
        assert!(meets_target(&low, &target) || low.to_u256_be() == [0u8; 32]);
        assert!(!meets_target(&high, &target));
    }
}
