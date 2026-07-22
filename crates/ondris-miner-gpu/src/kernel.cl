// OndrisHash GPU kernel.
//
// This is a mechanical transcription of the Rust reference chain that
// backs it: blake3_ref.rs -> xoshiro_ref.rs -> ondris_hash_ref.rs, each of
// which is unit-tested against the real, audited `blake3` / `rand_xoshiro`
// crates before ever reaching this file. If you change the algorithm here,
// change it there first and re-validate — this file itself is not
// independently tested against anything, only against the host-side
// known-answer harness in main.rs.

constant uint IV[8] = {
    0x6A09E667u, 0xBB67AE85u, 0x3C6EF372u, 0xA54FF53Au,
    0x510E527Fu, 0x9B05688Cu, 0x1F83D9ABu, 0x5BE0CD19u
};
constant int MSG_PERMUTATION[16] = { 2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8 };

#define CHUNK_START 1u
#define CHUNK_END   2u
#define PARENT      4u
#define ROOT        8u
#define CHUNK_LEN   1024
#define BLOCK_LEN   64
#define ITEM_SIZE   32
#define MAX_STACK   20

inline uint rotr32(uint x, uint n) {
    return rotate(x, 32u - n);
}

inline void g(uint *state, int a, int b, int c, int d, uint mx, uint my) {
    state[a] = state[a] + state[b] + mx;
    state[d] = rotr32(state[d] ^ state[a], 16u);
    state[c] = state[c] + state[d];
    state[b] = rotr32(state[b] ^ state[c], 12u);
    state[a] = state[a] + state[b] + my;
    state[d] = rotr32(state[d] ^ state[a], 8u);
    state[c] = state[c] + state[d];
    state[b] = rotr32(state[b] ^ state[c], 7u);
}

inline void compress(const uint cv[8], const uint block_words[16], ulong counter, uint block_len, uint flags, uint out16[16]) {
    uint state[16];
    for (int i = 0; i < 8; i++) state[i] = cv[i];
    state[8] = IV[0]; state[9] = IV[1]; state[10] = IV[2]; state[11] = IV[3];
    state[12] = (uint)(counter & 0xffffffffUL);
    state[13] = (uint)(counter >> 32);
    state[14] = block_len;
    state[15] = flags;

    uint m[16];
    for (int i = 0; i < 16; i++) m[i] = block_words[i];

    for (int round = 0; round < 7; round++) {
        g(state, 0, 4, 8, 12, m[0], m[1]);
        g(state, 1, 5, 9, 13, m[2], m[3]);
        g(state, 2, 6, 10, 14, m[4], m[5]);
        g(state, 3, 7, 11, 15, m[6], m[7]);
        g(state, 0, 5, 10, 15, m[8], m[9]);
        g(state, 1, 6, 11, 12, m[10], m[11]);
        g(state, 2, 7, 8, 13, m[12], m[13]);
        g(state, 3, 4, 9, 14, m[14], m[15]);

        if (round < 6) {
            uint permuted[16];
            for (int i = 0; i < 16; i++) permuted[i] = m[MSG_PERMUTATION[i]];
            for (int i = 0; i < 16; i++) m[i] = permuted[i];
        }
    }

    for (int i = 0; i < 8; i++) {
        out16[i] = state[i] ^ state[i + 8];
        out16[i + 8] = state[i + 8] ^ cv[i];
    }
}

inline void words_from_le_bytes_padded(const uchar *block, uint block_len, uint words[16]) {
    uchar padded[BLOCK_LEN];
    for (int i = 0; i < BLOCK_LEN; i++) padded[i] = ((uint)i < block_len) ? block[i] : 0;
    for (int i = 0; i < 16; i++) {
        words[i] = (uint)padded[i * 4] | ((uint)padded[i * 4 + 1] << 8) | ((uint)padded[i * 4 + 2] << 16) | ((uint)padded[i * 4 + 3] << 24);
    }
}

inline void chaining_value_of(const uint cv[8], const uint block_words[16], ulong counter, uint block_len, uint flags, uint out8[8]) {
    uint out16[16];
    compress(cv, block_words, counter, block_len, flags, out16);
    for (int i = 0; i < 8; i++) out8[i] = out16[i];
}

// `chunk` must be <= CHUNK_LEN (1024) bytes. `root` is true only when this
// chunk is the only one in the whole input.
inline void hash_chunk(const uchar *chunk, uint chunk_len, ulong chunk_counter, bool root, uint cv_out[8]) {
    uint cv[8];
    for (int i = 0; i < 8; i++) cv[i] = IV[i];

    if (chunk_len == 0) {
        uint flags = CHUNK_START | CHUNK_END | (root ? ROOT : 0u);
        uint zero_words[16];
        for (int i = 0; i < 16; i++) zero_words[i] = 0;
        chaining_value_of(cv, zero_words, chunk_counter, 0u, flags, cv_out);
        return;
    }

    uint n_blocks = (chunk_len + BLOCK_LEN - 1) / BLOCK_LEN;
    for (uint i = 0; i < n_blocks; i++) {
        uint off = i * BLOCK_LEN;
        uint this_len = min((uint)BLOCK_LEN, chunk_len - off);
        uint flags = 0u;
        if (i == 0u) flags |= CHUNK_START;
        if (i == n_blocks - 1u) {
            flags |= CHUNK_END;
            if (root) flags |= ROOT;
        }
        uint block_words[16];
        words_from_le_bytes_padded(chunk + off, this_len, block_words);
        uint next_cv[8];
        chaining_value_of(cv, block_words, chunk_counter, this_len, flags, next_cv);
        for (int k = 0; k < 8; k++) cv[k] = next_cv[k];
    }
    for (int i = 0; i < 8; i++) cv_out[i] = cv[i];
}

inline void parent_cv(const uint left[8], const uint right[8], bool root, uint out8[8]) {
    uint block_words[16];
    for (int i = 0; i < 8; i++) { block_words[i] = left[i]; block_words[8 + i] = right[i]; }
    uint iv[8];
    for (int i = 0; i < 8; i++) iv[i] = IV[i];
    uint flags = PARENT | (root ? ROOT : 0u);
    chaining_value_of(iv, block_words, 0UL, (uint)BLOCK_LEN, flags, out8);
}

inline void words_to_bytes(const uint w[8], uchar out[32]) {
    for (int i = 0; i < 8; i++) {
        out[i * 4 + 0] = (uchar)(w[i] & 0xffu);
        out[i * 4 + 1] = (uchar)((w[i] >> 8) & 0xffu);
        out[i * 4 + 2] = (uchar)((w[i] >> 16) & 0xffu);
        out[i * 4 + 3] = (uchar)((w[i] >> 24) & 0xffu);
    }
}

// data/len <= CHUNK_LEN (1024) — every call site in ondris_hash except the
// final scratchpad hash is guaranteed to be this small (fixed-size header
// input, fixed 64-byte mixing-round input), so this skips the tree/stack
// machinery entirely.
inline void blake3_hash_small(const uchar *data, uint len, uchar out[32]) {
    uint cv[8];
    hash_chunk(data, len, 0UL, true, cv);
    words_to_bytes(cv, out);
}

inline void merge_cv_stack(uint stack[MAX_STACK][8], int *stack_len, const uint new_cv[8], ulong total_chunks) {
    uint cur[8];
    for (int i = 0; i < 8; i++) cur[i] = new_cv[i];
    while ((total_chunks & 1UL) == 0UL) {
        (*stack_len)--;
        uint left[8];
        for (int i = 0; i < 8; i++) left[i] = stack[*stack_len][i];
        uint merged[8];
        parent_cv(left, cur, false, merged);
        for (int i = 0; i < 8; i++) cur[i] = merged[i];
        total_chunks >>= 1;
    }
    for (int i = 0; i < 8; i++) stack[*stack_len][i] = cur[i];
    (*stack_len)++;
}

// Full multi-chunk BLAKE3 over a `__global` buffer — used once per nonce,
// for the final scratchpad hash (which can be multiple megabytes, i.e.
// many chunks), unlike every other hash call in this file.
inline void blake3_hash_large(__global const uchar *data, uint len, uchar out[32]) {
    uchar local_buf[CHUNK_LEN];

    if (len <= CHUNK_LEN) {
        for (uint i = 0; i < len; i++) local_buf[i] = data[i];
        uint cv[8];
        hash_chunk(local_buf, len, 0UL, true, cv);
        words_to_bytes(cv, out);
        return;
    }

    uint stack[MAX_STACK][8];
    int stack_len = 0;
    uint offset = 0;
    ulong chunk_counter = 0;
    uint n_chunks = (len + CHUNK_LEN - 1) / CHUNK_LEN;

    while (chunk_counter + 1UL < (ulong)n_chunks) {
        for (uint i = 0; i < (uint)CHUNK_LEN; i++) local_buf[i] = data[offset + i];
        uint cv[8];
        hash_chunk(local_buf, (uint)CHUNK_LEN, chunk_counter, false, cv);
        merge_cv_stack(stack, &stack_len, cv, chunk_counter + 1UL);
        offset += (uint)CHUNK_LEN;
        chunk_counter++;
    }

    uint last_len = len - offset;
    for (uint i = 0; i < last_len; i++) local_buf[i] = data[offset + i];
    uint acc[8];
    hash_chunk(local_buf, last_len, chunk_counter, stack_len == 0, acc);

    while (stack_len > 0) {
        stack_len--;
        uint left[8];
        for (int i = 0; i < 8; i++) left[i] = stack[stack_len][i];
        bool is_root = (stack_len == 0);
        uint merged[8];
        parent_cv(left, acc, is_root, merged);
        for (int i = 0; i < 8; i++) acc[i] = merged[i];
    }

    words_to_bytes(acc, out);
}

// ---- xoshiro256** ----

typedef struct { ulong s[4]; } Xoshiro256ss;

inline void xoshiro_seed(Xoshiro256ss *rng, const uchar seed[32]) {
    for (int i = 0; i < 4; i++) {
        ulong w = 0;
        for (int b = 0; b < 8; b++) {
            w |= ((ulong)seed[i * 8 + b]) << (8 * b);
        }
        rng->s[i] = w;
    }
}

inline ulong xoshiro_next(Xoshiro256ss *rng) {
    ulong result = rotate(rng->s[1] * 5UL, 7UL) * 9UL;
    ulong t = rng->s[1] << 17;
    rng->s[2] ^= rng->s[0];
    rng->s[3] ^= rng->s[1];
    rng->s[1] ^= rng->s[2];
    rng->s[0] ^= rng->s[3];
    rng->s[2] ^= t;
    rng->s[3] = rotate(rng->s[3], 45UL);
    return result;
}

// ---- OndrisHash mining kernel ----
//
// One work-item = one nonce attempt = `nonce_base + get_global_id(0)`.
// Each work-item owns a private `scratchpad_size`-byte slice of
// `scratchpad_pool` (there is no way to fit a multi-megabyte scratchpad in
// genuinely private/local GPU memory, so it lives in a global buffer
// instead, sized `batch_size * scratchpad_size` by the host).
__kernel void ondris_mine(
    __global const uchar *dataset,
    ulong dataset_len,
    __global const uchar *header_bytes,
    uint header_len,
    ulong nonce_base,
    uint scratchpad_size,
    uint mix_rounds,
    __global uchar *scratchpad_pool,
    __global const uchar *target,
    __global ulong *result_nonce,
    __global int *result_found
) {
    size_t gid = get_global_id(0);
    ulong nonce = nonce_base + (ulong)gid;

    uchar input_buf[256];
    for (uint i = 0; i < header_len; i++) input_buf[i] = header_bytes[i];
    for (int i = 0; i < 8; i++) input_buf[header_len + i] = (uchar)((nonce >> (8 * i)) & 0xffUL);
    uint total_len = header_len + 8u;

    uchar seed[32];
    blake3_hash_small(input_buf, total_len, seed);

    Xoshiro256ss rng;
    xoshiro_seed(&rng, seed);

    uint n_blocks = scratchpad_size / ITEM_SIZE;
    ulong n_items = dataset_len / ITEM_SIZE;

    __global uchar *scratchpad = scratchpad_pool + (size_t)gid * (size_t)scratchpad_size;

    for (uint b = 0; b < n_blocks; b++) {
        ulong idx = xoshiro_next(&rng) % n_items;
        __global const uchar *item = dataset + idx * ITEM_SIZE;
        uint off = b * ITEM_SIZE;
        for (uint k = 0; k < ITEM_SIZE; k++) {
            scratchpad[off + k] = item[k] ^ seed[k];
        }
    }

    for (uint round = 0; round < mix_rounds; round++) {
        for (uint b = 0; b < n_blocks; b++) {
            ulong dep_idx = xoshiro_next(&rng) % (ulong)n_blocks;
            uint off = b * ITEM_SIZE;
            uint dep_off = (uint)dep_idx * ITEM_SIZE;

            uchar buf[BLOCK_LEN];
            for (uint k = 0; k < ITEM_SIZE; k++) buf[k] = scratchpad[off + k];
            for (uint k = 0; k < ITEM_SIZE; k++) buf[ITEM_SIZE + k] = scratchpad[dep_off + k];

            uchar out[32];
            blake3_hash_small(buf, (uint)BLOCK_LEN, out);

            for (uint k = 0; k < ITEM_SIZE; k++) scratchpad[off + k] = out[k];
        }
    }

    uchar final_hash[32];
    blake3_hash_large(scratchpad, scratchpad_size, final_hash);

    bool meets = true;
    for (int i = 0; i < 32; i++) {
        if (final_hash[i] < target[i]) { meets = true; break; }
        if (final_hash[i] > target[i]) { meets = false; break; }
    }

    if (meets) {
        *result_found = 1;
        *result_nonce = nonce;
    }
}

// Debug-only kernel exposed for the host-side known-answer test harness:
// computes OndrisHash for a single nonce and writes the raw 32-byte
// digest out, instead of only a target comparison. Lets main.rs validate
// the kernel bit-for-bit against `ondris_pow::ondris_hash_with_sizes`
// before ever trusting it to look for a real target match.
__kernel void ondris_hash_debug(
    __global const uchar *dataset,
    ulong dataset_len,
    __global const uchar *header_bytes,
    uint header_len,
    ulong nonce,
    uint scratchpad_size,
    uint mix_rounds,
    __global uchar *scratchpad_pool,
    __global uchar *digest_out
) {
    uchar input_buf[256];
    for (uint i = 0; i < header_len; i++) input_buf[i] = header_bytes[i];
    for (int i = 0; i < 8; i++) input_buf[header_len + i] = (uchar)((nonce >> (8 * i)) & 0xffUL);
    uint total_len = header_len + 8u;

    uchar seed[32];
    blake3_hash_small(input_buf, total_len, seed);

    Xoshiro256ss rng;
    xoshiro_seed(&rng, seed);

    uint n_blocks = scratchpad_size / ITEM_SIZE;
    ulong n_items = dataset_len / ITEM_SIZE;

    __global uchar *scratchpad = scratchpad_pool;

    for (uint b = 0; b < n_blocks; b++) {
        ulong idx = xoshiro_next(&rng) % n_items;
        __global const uchar *item = dataset + idx * ITEM_SIZE;
        uint off = b * ITEM_SIZE;
        for (uint k = 0; k < ITEM_SIZE; k++) {
            scratchpad[off + k] = item[k] ^ seed[k];
        }
    }

    for (uint round = 0; round < mix_rounds; round++) {
        for (uint b = 0; b < n_blocks; b++) {
            ulong dep_idx = xoshiro_next(&rng) % (ulong)n_blocks;
            uint off = b * ITEM_SIZE;
            uint dep_off = (uint)dep_idx * ITEM_SIZE;

            uchar buf[BLOCK_LEN];
            for (uint k = 0; k < ITEM_SIZE; k++) buf[k] = scratchpad[off + k];
            for (uint k = 0; k < ITEM_SIZE; k++) buf[ITEM_SIZE + k] = scratchpad[dep_off + k];

            uchar out[32];
            blake3_hash_small(buf, (uint)BLOCK_LEN, out);

            for (uint k = 0; k < ITEM_SIZE; k++) scratchpad[off + k] = out[k];
        }
    }

    uchar final_hash[32];
    blake3_hash_large(scratchpad, scratchpad_size, final_hash);
    for (int i = 0; i < 32; i++) digest_out[i] = final_hash[i];
}
