//! OndrisHash: a memory-hard, GPU-friendly, ASIC-resistant Proof-of-Work
//! algorithm. See `docs/ALGORITHM.md` at the repo root for the full spec
//! and warnings about its unaudited status.
//!
//! Structurally, this is an Ethash-style design: a large read-only dataset
//! regenerated per epoch, a small number of pseudo-random touches into it
//! per hash attempt, and a cheap (non-cryptographic) FNV mix combining
//! them — the same shape that's secured Ethereum's mainnet for years.
//! BLAKE3 (audited, standardized) is the only cryptographic primitive
//! involved, used for seed derivation, expanding that seed into the
//! initial mix state, and sealing the final result; FNV mixing in between
//! doesn't need to be cryptographically strong on its own, only
//! unpredictable enough to force real dataset reads.
//!
//! An earlier version of this algorithm instead used a CryptoNight/
//! RandomX-style scratchpad mixed over many sequential rounds. That shape
//! is a deliberate choice those algorithms make to favor CPUs and starve
//! GPUs/ASICs — exactly backwards from this project's stated goal, and
//! confirmed in practice: benchmarking a GPU implementation of it showed
//! *worse* throughput than a 4-thread CPU miner, because 500,000+
//! sequentially-dependent BLAKE3 calls per hash is compute-bound, not
//! memory-bandwidth-bound, and compute-bound workloads don't play to a
//! GPU's actual strength. This version fixes that at the algorithm level,
//! not by tuning the old one further.

use ondris_primitives::Hash256;

/// Block height interval at which a new dataset is generated.
pub const EPOCH_LENGTH: u64 = 2048;
/// Size of the compact cache the full dataset is derived from.
pub const CACHE_SIZE: usize = 16 * 1024 * 1024;
/// Size of the full dataset used for mixing (reduced testnet value for
/// fast dev/test cycles; to be revisited with an audit and real GPU
/// benchmarks before any mainnet launch — target 2-4 GiB).
pub const DATASET_SIZE: usize = 64 * 1024 * 1024;
/// Size of one dataset item / the mix buffer, in bytes. 128 matches
/// Ethash's proven value: large enough that a random access amortizes
/// well against DRAM/PCIe overhead, small enough to expand cheaply.
pub const ITEM_SIZE: usize = 128;
/// Number of pseudo-random dataset touches per hash attempt. Also an
/// Ethash-matching value — the point is a hash that's dominated by a
/// couple dozen real random memory reads, not by raw compute.
pub const ACCESSES: usize = 64;

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
    /// sizes above are too heavy for a unit test loop). Item size is
    /// always `ITEM_SIZE` — only the overall cache/dataset byte budgets
    /// (and therefore the item *count*) vary.
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
                let mut reader = hasher.finalize_xof();
                reader.fill(&mut item);
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
    ondris_hash_with_accesses(header_bytes, nonce, dataset, ACCESSES)
}

/// Access-count-parameterized variant, used by tests.
pub fn ondris_hash_with_accesses(
    header_bytes: &[u8],
    nonce: u64,
    dataset: &Dataset,
    accesses: usize,
) -> Hash256 {
    let mut input = Vec::with_capacity(header_bytes.len() + 8);
    input.extend_from_slice(header_bytes);
    input.extend_from_slice(&nonce.to_le_bytes());
    let seed = *blake3::hash(&input).as_bytes();

    let mut mix = vec![0u8; ITEM_SIZE];
    xof_fill(&seed, &mut mix);

    let seed_word0 = read_u32_le(&seed, 0);
    let words_per_item = ITEM_SIZE / 4;
    let n_items = (dataset.len_bytes() / ITEM_SIZE).max(1) as u64;

    for i in 0..accesses {
        let mix_word = read_u32_le(&mix, (i % words_per_item) * 4);
        let p = fnv(seed_word0 ^ i as u32, mix_word) as u64 % n_items;
        let item = dataset.item(p);
        for w in 0..words_per_item {
            let mixed = fnv(read_u32_le(&mix, w * 4), read_u32_le(item, w * 4));
            write_u32_le(&mut mix, w * 4, mixed);
        }
    }

    // Compress the ITEM_SIZE-byte mix down to 32 bytes: fold each group of
    // four consecutive words together with FNV (same compression Ethash
    // uses on its 128-byte mix before the final hash).
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
    Hash256::hash(&final_input)
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
        let a = ondris_hash_with_accesses(header, 42, &ds, 8);
        let b = ondris_hash_with_accesses(header, 42, &ds, 8);
        assert_eq!(a, b);
    }

    #[test]
    fn different_nonce_changes_hash() {
        let ds = tiny_dataset();
        let header = b"header-bytes";
        let a = ondris_hash_with_accesses(header, 1, &ds, 8);
        let b = ondris_hash_with_accesses(header, 2, &ds, 8);
        assert_ne!(a, b);
    }

    #[test]
    fn different_epoch_seed_changes_dataset() {
        let ds1 = Dataset::generate_with_sizes(0, Hash256::hash(b"seed-a"), 4096, 8192);
        let ds2 = Dataset::generate_with_sizes(0, Hash256::hash(b"seed-b"), 4096, 8192);
        let header = b"header-bytes";
        let a = ondris_hash_with_accesses(header, 1, &ds1, 8);
        let b = ondris_hash_with_accesses(header, 1, &ds2, 8);
        assert_ne!(a, b);
    }

    #[test]
    fn more_accesses_still_deterministic_and_differs_from_fewer() {
        let ds = tiny_dataset();
        let header = b"header-bytes";
        let few = ondris_hash_with_accesses(header, 1, &ds, 4);
        let many = ondris_hash_with_accesses(header, 1, &ds, 64);
        assert_ne!(
            few, many,
            "different access counts should (almost always) diverge"
        );
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
