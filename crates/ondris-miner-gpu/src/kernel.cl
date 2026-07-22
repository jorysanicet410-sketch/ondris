// OndrisHash GPU kernel — Ethash-style design: a large read-only dataset,
// a handful of pseudo-random touches into it per hash, cheap FNV mixing.
//
// This is a mechanical transcription of the Rust reference chain that
// backs it: blake3_ref.rs -> ondris_hash_ref.rs, each of which is
// unit-tested against the real, audited `blake3` crate before ever
// reaching this file. If you change the algorithm here, change it there
// first and re-validate — this file itself is not independently tested
// against anything, only against the host-side known-answer harness in
// main.rs (`ondris-miner-gpu self-test`).
//
// Notably absent compared to an earlier version of this kernel: a
// per-work-item multi-megabyte scratchpad, and the multi-chunk BLAKE3
// tree-hashing machinery that came with hashing it. Neither the header+
// nonce input nor the final-hash input here ever exceeds 64 bytes, so a
// single-chunk, single-or-double-block BLAKE3 call covers every hash in
// this file — no chunking, no CV stack.

constant uint IV[8] = {
    0x6A09E667u, 0xBB67AE85u, 0x3C6EF372u, 0xA54FF53Au,
    0x510E527Fu, 0x9B05688Cu, 0x1F83D9ABu, 0x5BE0CD19u
};
constant int MSG_PERMUTATION[16] = { 2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8 };

#define CHUNK_START 1u
#define CHUNK_END   2u
#define ROOT        8u
#define BLOCK_LEN   64
#define ITEM_SIZE   128
#define FNV_PRIME   0x01000193u

inline uint rotr32(uint x, uint n) {
    return rotate(x, 32u - n);
}

#define G(a, b, c, d, mx, my) do { \
    state[a] = state[a] + state[b] + (mx); \
    state[d] = rotr32(state[d] ^ state[a], 16u); \
    state[c] = state[c] + state[d]; \
    state[b] = rotr32(state[b] ^ state[c], 12u); \
    state[a] = state[a] + state[b] + (my); \
    state[d] = rotr32(state[d] ^ state[a], 8u); \
    state[c] = state[c] + state[d]; \
    state[b] = rotr32(state[b] ^ state[c], 7u); \
} while (0)

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
        G(0, 4, 8, 12, m[0], m[1]);
        G(1, 5, 9, 13, m[2], m[3]);
        G(2, 6, 10, 14, m[4], m[5]);
        G(3, 7, 11, 15, m[6], m[7]);
        G(0, 5, 10, 15, m[8], m[9]);
        G(1, 6, 11, 12, m[10], m[11]);
        G(2, 7, 8, 13, m[12], m[13]);
        G(3, 4, 9, 14, m[14], m[15]);

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
    if (block_len == (uint)BLOCK_LEN) {
        for (int i = 0; i < 16; i++) {
            words[i] = (uint)block[i * 4] | ((uint)block[i * 4 + 1] << 8) | ((uint)block[i * 4 + 2] << 16) | ((uint)block[i * 4 + 3] << 24);
        }
        return;
    }
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

inline void words_to_bytes(const uint w[8], uchar out[32]) {
    for (int i = 0; i < 8; i++) {
        out[i * 4 + 0] = (uchar)(w[i] & 0xffu);
        out[i * 4 + 1] = (uchar)((w[i] >> 8) & 0xffu);
        out[i * 4 + 2] = (uchar)((w[i] >> 16) & 0xffu);
        out[i * 4 + 3] = (uchar)((w[i] >> 24) & 0xffu);
    }
}

// Plain BLAKE3 hash of a single-chunk (<=1024 byte, in practice always
// <=64 bytes in this file) private-memory input.
inline void blake3_hash_small(const uchar *data, uint len, uchar out[32]) {
    uint cv[8];
    for (int i = 0; i < 8; i++) cv[i] = IV[i];

    if (len == 0) {
        uint zero_words[16];
        for (int i = 0; i < 16; i++) zero_words[i] = 0;
        chaining_value_of(cv, zero_words, 0UL, 0u, CHUNK_START | CHUNK_END | ROOT, cv);
        words_to_bytes(cv, out);
        return;
    }

    uint n_blocks = (len + BLOCK_LEN - 1) / BLOCK_LEN;
    for (uint i = 0; i < n_blocks; i++) {
        uint off = i * BLOCK_LEN;
        uint this_len = min((uint)BLOCK_LEN, len - off);
        uint flags = 0u;
        if (i == 0u) flags |= CHUNK_START;
        if (i == n_blocks - 1u) flags |= CHUNK_END | ROOT;
        uint block_words[16];
        words_from_le_bytes_padded(data + off, this_len, block_words);
        chaining_value_of(cv, block_words, 0UL, this_len, flags, cv);
    }
    words_to_bytes(cv, out);
}

// BLAKE3's XOF (extendable-output) mode, narrowed to the one case this
// kernel needs: expanding a 32-byte seed into ITEM_SIZE (128) bytes. A
// 32-byte input is always exactly one block of one chunk, so the "root
// node" is just that single block, computed once — real BLAKE3 XOF
// replays that same root compression with an incrementing counter to
// produce each successive 64-byte output block (not a simplification
// specific to this case; it's the actual mechanism).
inline void xof_from_32_bytes(const uchar seed[32], uchar out[ITEM_SIZE]) {
    uint block_words[16];
    words_from_le_bytes_padded(seed, 32u, block_words);
    uint flags = CHUNK_START | CHUNK_END | ROOT;
    // `compress()` takes `cv` in the default (private/generic) address
    // space, so `IV` (declared `constant`) has to be copied into a local
    // array first — NVIDIA's OpenCL compiler rejects passing a
    // `__constant uint*` where a `const uint*` is expected, even though
    // both are read-only.
    uint iv_local[8];
    for (int i = 0; i < 8; i++) iv_local[i] = IV[i];

    uint written = 0;
    ulong counter = 0;
    while (written < (uint)ITEM_SIZE) {
        uint out16[16];
        compress(iv_local, block_words, counter, 32u, flags, out16);
        for (int i = 0; i < 16 && written < (uint)ITEM_SIZE; i++) {
            out[written + 0] = (uchar)(out16[i] & 0xffu);
            out[written + 1] = (uchar)((out16[i] >> 8) & 0xffu);
            out[written + 2] = (uchar)((out16[i] >> 16) & 0xffu);
            out[written + 3] = (uchar)((out16[i] >> 24) & 0xffu);
            written += 4;
        }
        counter++;
    }
}

inline uint fnv(uint a, uint b) {
    return (a * FNV_PRIME) ^ b;
}

inline uint read_u32(const uchar *p) {
    return (uint)p[0] | ((uint)p[1] << 8) | ((uint)p[2] << 16) | ((uint)p[3] << 24);
}

inline uint read_u32_global(__global const uchar *p) {
    return (uint)p[0] | ((uint)p[1] << 8) | ((uint)p[2] << 16) | ((uint)p[3] << 24);
}

inline void write_u32(uchar *p, uint v) {
    p[0] = (uchar)(v & 0xffu);
    p[1] = (uchar)((v >> 8) & 0xffu);
    p[2] = (uchar)((v >> 16) & 0xffu);
    p[3] = (uchar)((v >> 24) & 0xffu);
}

// The whole algorithm, shared by both kernels below. `header_bytes` is
// read directly from global memory (it's tiny and read-only, no reason to
// stage it through a private copy first).
inline void ondris_hash_core(
    __global const uchar *dataset,
    ulong dataset_len,
    __global const uchar *header_bytes,
    uint header_len,
    ulong nonce,
    uint accesses,
    uchar out[32]
) {
    uchar input_buf[144];
    for (uint i = 0; i < header_len; i++) input_buf[i] = header_bytes[i];
    for (int i = 0; i < 8; i++) input_buf[header_len + i] = (uchar)((nonce >> (8 * i)) & 0xffUL);
    uint total_len = header_len + 8u;

    uchar seed[32];
    blake3_hash_small(input_buf, total_len, seed);

    uchar mix[ITEM_SIZE];
    xof_from_32_bytes(seed, mix);

    uint seed_word0 = read_u32(seed);
    uint words_per_item = ITEM_SIZE / 4;
    ulong n_items = dataset_len / ITEM_SIZE;

    for (uint i = 0; i < accesses; i++) {
        uint mix_word = read_u32(mix + (i % words_per_item) * 4);
        ulong p = (ulong)fnv(seed_word0 ^ i, mix_word) % n_items;
        __global const uchar *item = dataset + p * (ulong)ITEM_SIZE;
        for (uint w = 0; w < words_per_item; w++) {
            uint mixed = fnv(read_u32(mix + w * 4), read_u32_global(item + w * 4));
            write_u32(mix + w * 4, mixed);
        }
    }

    uchar compressed[32];
    for (int i = 0; i < 8; i++) {
        uint base = i * 16;
        uint w0 = read_u32(mix + base);
        uint w1 = read_u32(mix + base + 4);
        uint w2 = read_u32(mix + base + 8);
        uint w3 = read_u32(mix + base + 12);
        write_u32(compressed + i * 4, fnv(fnv(fnv(w0, w1), w2), w3));
    }

    uchar final_input[64];
    for (int i = 0; i < 32; i++) final_input[i] = seed[i];
    for (int i = 0; i < 32; i++) final_input[32 + i] = compressed[i];
    blake3_hash_small(final_input, 64u, out);
}

// One work-item = one nonce attempt = `nonce_base + get_global_id(0)`.
// Unlike the scratchpad-mixing design this replaced, there is no
// per-work-item writable buffer at all here — `mix` is 128 bytes of
// private memory, and the only large buffer is the read-only, shared
// `dataset`. That's what makes a much larger batch size than before
// practical: batch size is no longer bounded by a `batch_size *
// multi-megabyte-scratchpad` global allocation.
__kernel void ondris_mine(
    __global const uchar *dataset,
    ulong dataset_len,
    __global const uchar *header_bytes,
    uint header_len,
    ulong nonce_base,
    uint accesses,
    __global const uchar *target,
    __global ulong *result_nonce,
    __global int *result_found
) {
    size_t gid = get_global_id(0);
    ulong nonce = nonce_base + (ulong)gid;

    uchar final_hash[32];
    ondris_hash_core(dataset, dataset_len, header_bytes, header_len, nonce, accesses, final_hash);

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
// the kernel bit-for-bit against `ondris_pow::ondris_hash` before ever
// trusting it to look for a real target match.
__kernel void ondris_hash_debug(
    __global const uchar *dataset,
    ulong dataset_len,
    __global const uchar *header_bytes,
    uint header_len,
    ulong nonce,
    uint accesses,
    __global uchar *digest_out
) {
    uchar final_hash[32];
    ondris_hash_core(dataset, dataset_len, header_bytes, header_len, nonce, accesses, final_hash);
    for (int i = 0; i < 32; i++) digest_out[i] = final_hash[i];
}
