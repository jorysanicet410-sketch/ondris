//! A from-scratch reimplementation of plain (unkeyed, non-XOF) BLAKE3
//! hashing, single-shot only (the whole input is available upfront — we
//! never need BLAKE3's incremental `update()` API since every call site
//! in `ondris_pow::ondris_hash` already has its full input in hand).
//!
//! This exists purely as a stepping stone: getting BLAKE3's chunk/tree
//! structure right is easy to get subtly wrong, and OpenCL C is a
//! miserable place to debug that. So the algorithm gets nailed down here
//! first — validated byte-for-byte against the real, audited `blake3`
//! crate in the tests below — and only then transcribed mechanically into
//! `kernel.cl`. This module is never used for anything consensus-critical
//! on the Rust side; `ondris-pow` keeps using the real crate.

const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

const MSG_PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

const CHUNK_START: u32 = 1 << 0;
const CHUNK_END: u32 = 1 << 1;
const PARENT: u32 = 1 << 2;
const ROOT: u32 = 1 << 3;

const CHUNK_LEN: usize = 1024;
const BLOCK_LEN: usize = 64;

fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(mx);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(my);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

/// The core compression function. Returns the full 16-word output state;
/// callers take either the first 8 words (as the next chaining value) or
/// XOR the two halves together (for a finalized 32-byte output).
fn compress(
    cv: [u32; 8],
    block_words: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 16] {
    let mut state = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];
    let mut m = block_words;

    for round in 0..7 {
        g(&mut state, 0, 4, 8, 12, m[0], m[1]);
        g(&mut state, 1, 5, 9, 13, m[2], m[3]);
        g(&mut state, 2, 6, 10, 14, m[4], m[5]);
        g(&mut state, 3, 7, 11, 15, m[6], m[7]);
        g(&mut state, 0, 5, 10, 15, m[8], m[9]);
        g(&mut state, 1, 6, 11, 12, m[10], m[11]);
        g(&mut state, 2, 7, 8, 13, m[12], m[13]);
        g(&mut state, 3, 4, 9, 14, m[14], m[15]);

        if round < 6 {
            let mut permuted = [0u32; 16];
            for i in 0..16 {
                permuted[i] = m[MSG_PERMUTATION[i]];
            }
            m = permuted;
        }
    }

    for i in 0..8 {
        state[i] ^= state[i + 8];
        state[i + 8] ^= cv[i];
    }
    state
}

fn words_from_le_bytes_padded(block: &[u8]) -> [u32; 16] {
    debug_assert!(block.len() <= BLOCK_LEN);
    let mut padded = [0u8; BLOCK_LEN];
    padded[..block.len()].copy_from_slice(block);
    let mut words = [0u32; 16];
    for (i, w) in words.iter_mut().enumerate() {
        *w = u32::from_le_bytes(padded[i * 4..i * 4 + 4].try_into().unwrap());
    }
    words
}

fn chaining_value_of(
    cv: [u32; 8],
    block_words: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 8] {
    let out = compress(cv, block_words, counter, block_len, flags);
    [
        out[0], out[1], out[2], out[3], out[4], out[5], out[6], out[7],
    ]
}

/// Hashes one chunk (up to 1024 bytes) of the overall input, returning its
/// chaining value. `root` must be true only when this chunk is the ONLY
/// chunk in the whole input (so its final block also gets the ROOT flag).
fn hash_chunk(chunk: &[u8], chunk_counter: u64, root: bool) -> [u32; 8] {
    debug_assert!(chunk.len() <= CHUNK_LEN);
    let cv = IV;
    if chunk.is_empty() {
        // BLAKE3 of the empty input is still one (empty) block, with
        // CHUNK_START|CHUNK_END|ROOT all set — `chunk.chunks(64)` on an
        // empty slice yields zero iterations, so this needs handling
        // explicitly rather than falling through the loop below.
        let flags = CHUNK_START | CHUNK_END | if root { ROOT } else { 0 };
        return chaining_value_of(cv, [0u32; 16], chunk_counter, 0, flags);
    }
    let mut cv = cv;
    let n_blocks = chunk.len().div_ceil(BLOCK_LEN);
    for (i, block) in chunk.chunks(BLOCK_LEN).enumerate() {
        let is_first = i == 0;
        let is_last = i == n_blocks - 1;
        let mut flags = 0u32;
        if is_first {
            flags |= CHUNK_START;
        }
        if is_last {
            flags |= CHUNK_END;
            if root {
                flags |= ROOT;
            }
        }
        let block_words = words_from_le_bytes_padded(block);
        cv = chaining_value_of(cv, block_words, chunk_counter, block.len() as u32, flags);
    }
    cv
}

fn parent_cv(left: [u32; 8], right: [u32; 8], root: bool) -> [u32; 8] {
    let mut block_words = [0u32; 16];
    block_words[..8].copy_from_slice(&left);
    block_words[8..].copy_from_slice(&right);
    let flags = PARENT | if root { ROOT } else { 0 };
    chaining_value_of(IV, block_words, 0, BLOCK_LEN as u32, flags)
}

/// Same tree-merge order as the real BLAKE3: chunk chaining values are
/// pushed onto a stack, and merged pairwise bottom-up whenever the total
/// chunk count so far has a trailing run of set bits — the same
/// bookkeeping as binary-counter carries. This guarantees the same
/// (unbalanced-when-needed) tree shape the reference implementation uses.
fn merge_cv_stack(stack: &mut Vec<[u32; 8]>, mut new_cv: [u32; 8], mut total_chunks: u64) {
    while total_chunks & 1 == 0 {
        let left = stack.pop().expect("stack has a pending left sibling");
        new_cv = parent_cv(left, new_cv, false);
        total_chunks >>= 1;
    }
    stack.push(new_cv);
}

pub fn hash(data: &[u8]) -> [u8; 32] {
    if data.len() <= CHUNK_LEN {
        let cv = hash_chunk(data, 0, true);
        return words_to_le_bytes(cv);
    }

    let mut stack: Vec<[u32; 8]> = Vec::new();
    let mut offset = 0usize;
    let mut chunk_counter = 0u64;
    let n_chunks = data.len().div_ceil(CHUNK_LEN) as u64;

    // Stream every chunk EXCEPT the last one through the normal
    // (never-root) merge logic. The real BLAKE3 reference does the same
    // thing for exactly this reason: `merge_cv_stack` doesn't know
    // whether the chunk it's merging is the last one, so if it were
    // allowed to fold the stack all the way down to one entry on its
    // own, the root flag would never get applied anywhere (this was the
    // actual bug the first version of this function had — it only
    // showed up when the total chunk count was an exact power of two,
    // e.g. exactly 2 chunks, because that's when merge_cv_stack's
    // internal collapse reaches exactly one leftover entry by itself).
    while chunk_counter + 1 < n_chunks {
        let end = offset + CHUNK_LEN;
        let cv = hash_chunk(&data[offset..end], chunk_counter, false);
        merge_cv_stack(&mut stack, cv, chunk_counter + 1);
        offset = end;
        chunk_counter += 1;
    }

    // The last chunk is folded in by hand instead, so whichever compress
    // call turns out to be the very last one of the whole hash — the
    // last chunk's own finalization if there's nothing left on the
    // stack, or the last parent merge otherwise — is the one that gets
    // the root flag.
    let last_chunk = &data[offset..data.len()];
    let mut acc = hash_chunk(last_chunk, chunk_counter, stack.is_empty());
    while let Some(left) = stack.pop() {
        let is_root = stack.is_empty();
        acc = parent_cv(left, acc, is_root);
    }
    words_to_le_bytes(acc)
}

fn words_to_le_bytes(words: [u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(data: &[u8]) {
        let expected = blake3::hash(data);
        let got = hash(data);
        assert_eq!(
            got,
            *expected.as_bytes(),
            "mismatch for input length {}",
            data.len()
        );
    }

    #[test]
    fn matches_real_blake3_across_size_classes() {
        check(b"");
        check(b"a");
        check(b"ondris");

        let pattern: Vec<u8> = (0..).map(|i: u32| (i % 251) as u8).take(4096).collect();

        for &len in &[
            1usize, 32, 63, 64, 65, 100, 128, 200, 512, 1023, 1024, 1025, 1500, 2000, 2048, 2049,
            3000, 4096,
        ] {
            check(&pattern[..len]);
        }
    }

    /// Exact chunk-count boundaries, including several exact powers of
    /// two — this is precisely the class of input that broke an earlier
    /// version of `hash()` (see the comment in that function).
    #[test]
    fn matches_real_blake3_at_exact_chunk_count_boundaries() {
        let pattern: Vec<u8> = (0..)
            .map(|i: u32| (i % 251) as u8)
            .take(20 * CHUNK_LEN)
            .collect();
        for chunks in 1u64..=16 {
            let len = (chunks as usize) * CHUNK_LEN;
            check(&pattern[..len]);
            if len > 1 {
                check(&pattern[..len - 1]);
            }
        }
    }

    #[test]
    fn matches_real_blake3_for_random_inputs() {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        for _ in 0..50 {
            let len = (rng.next_u32() % 8192) as usize;
            let mut buf = vec![0u8; len];
            rng.fill_bytes(&mut buf);
            check(&buf);
        }
    }

    #[test]
    fn matches_real_blake3_for_large_multichunk_input() {
        let data: Vec<u8> = (0..2_000_000u32).map(|i| (i % 256) as u8).collect();
        check(&data);
        check(&data[..2_000_001.min(data.len())]);
    }
}
