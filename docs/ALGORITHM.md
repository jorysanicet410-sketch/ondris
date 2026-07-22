# OndrisHash — Proof-of-Work Algorithm

## Status

**Unaudited.** This spec and its reference implementation have not yet been
reviewed by independent cryptographers. Do not trust it with real value
before an external audit. OndrisHash does not reinvent any cryptographic
primitive: it combines BLAKE3 (an audited, standardized hash function) with
a non-cryptographic FNV mix in an Ethash-style architecture. What's new
here is the parameterization and BLAKE3 in place of Keccak — not a new
cryptographic construction and not a new consensus shape.

## Revision history

**v2 (current).** An Ethash-style design: a large read-only dataset, a
small fixed number of pseudo-random touches into it per hash (64), and a
cheap FNV mix combining them.

**v1 (retired).** The original design used a CryptoNight/RandomX-style
scratchpad, mixed over many sequential rounds (500,000+ BLAKE3 calls per
hash). That shape is a deliberate choice those algorithms make to favor
CPUs and starve GPUs and ASICs — which is exactly backwards from this
project's stated goal. This was confirmed empirically, not just
theoretically: a real OpenCL implementation of v1 benchmarked at ~75 H/s
on an NVIDIA RTX 4070 Super, *slower* than a 4-thread CPU miner (~137 H/s)
on the same machine. The root cause was architectural — 500,000+
sequentially-dependent hash calls per attempt is a compute-bound workload,
and compute-bound workloads don't play to a GPU's actual strength (memory
bandwidth) — so it was fixed by changing the algorithm, not by tuning the
GPU kernel further. v2's real-hardware benchmark on the same GPU: **~13
million H/s**, roughly 95,000x faster than v1 and ~95,000x faster than the
CPU miner. This is a breaking, consensus-level change — v1 and v2 chains
are entirely incompatible with each other.

## Design goals

1. **GPU-friendly**: dominated by random reads from a large shared
   dataset, which plays to a GPU's actual strength (high aggregate memory
   bandwidth, thousands of concurrent threads) rather than raw
   single-thread compute.
2. **ASIC-resistant**: mining requires holding a multi-hundred-MB-to-GB
   dataset in fast memory. A dedicated ASIC would need to embed the same
   amount of fast RAM a GPU already ships with, which cancels out its
   usual cost/power advantage. (Historical honesty: Ethash itself, which
   this design is structurally modeled on, was eventually beaten by
   dedicated ASICs after a few years in production. "ASIC-resistant" is
   not a permanent property of any memory-hard scheme — it raises the
   cost and delays the point where dedicated hardware becomes worthwhile,
   it doesn't prevent it forever.)
3. **CPU-uncompetitive by design**: a CPU can still compute the algorithm
   (needed for node-side verification, which must work without a GPU),
   but its throughput is not meant to be competitive with a GPU's for
   actual mining — this is the intended, working consequence of the
   design, not an oversight.

## Parameters

| Constant | Testnet value | Description |
|---|---|---|
| `EPOCH_LENGTH` | 2048 blocks | How often the dataset is regenerated |
| `CACHE_SIZE` | 16 MiB | Compact seed the full dataset is derived from |
| `DATASET_SIZE` | 64 MiB (testnet/dev) / 2-4 GiB (mainnet target) | Full read-only dataset |
| `ITEM_SIZE` | 128 bytes | Size of one dataset item / the mix buffer — matches Ethash's proven value |
| `ACCESSES` | 64 | Pseudo-random dataset touches per hash attempt — also Ethash-matching |

Testnet sizes are intentionally reduced so development and tests run fast
on modest hardware. Mainnet values will be revisited with the auditor
before any real launch.

## Step 1 — Epoch seed

Unchanged from v1:

```
epoch(height) = height / EPOCH_LENGTH
epoch_seed(0) = BLAKE3("ONDRIS_GENESIS_EPOCH")
epoch_seed(e) = BLAKE3(hash_of_block_at(e * EPOCH_LENGTH))   for e > 0
```

The epoch seed depends on the actual content of the chain, which prevents
precomputing future datasets ahead of time.

## Step 2 — Dataset

```
cache = BLAKE3_XOF(epoch_seed, output_len = CACHE_SIZE)

dataset[i] for i in [0, DATASET_SIZE / ITEM_SIZE):
    item = cache[(i * ITEM_SIZE) % CACHE_SIZE .. +ITEM_SIZE]
    repeat 2 times:
        item = BLAKE3_XOF(item || i.to_le_bytes(), output_len = ITEM_SIZE)
    dataset[i*ITEM_SIZE .. +ITEM_SIZE] = item
```

Same shape as v1 (cache expansion + per-item re-hashing), just with
128-byte items instead of 32-byte ones. Generated once per epoch by every
node and miner from the small `cache` — never transferred over the
network.

## Step 3 — Hashing one attempt (header + nonce)

```
input = header_bytes || nonce.to_le_bytes()
seed  = BLAKE3(input)                              // 32 bytes
mix   = BLAKE3_XOF(seed, output_len = ITEM_SIZE)    // 128 bytes

seed_word0 = seed[0..4] as u32 (little-endian)

for i in 0..ACCESSES:
    mix_word = mix[(i % (ITEM_SIZE/4)) * 4 .. +4] as u32
    p        = fnv(seed_word0 XOR i, mix_word) mod (DATASET_SIZE / ITEM_SIZE)
    item     = dataset[p * ITEM_SIZE .. +ITEM_SIZE]
    for each 4-byte word w at position k in mix and item:
        mix[k] = fnv(mix[k], item[k])

// Compress the 128-byte mix down to 32 bytes: fold each group of four
// consecutive words with fnv (same compression Ethash uses).
compressed = fold_fnv(mix)   // 32 bytes

final_hash = BLAKE3(seed || compressed)   // 32 bytes
```

Where `fnv(a, b) = (a * 0x01000193) XOR b` — the exact FNV-1 variant
Ethash itself uses for this purpose. It's intentionally not
cryptographically strong on its own: its job is to be cheap and to force a
real dependency on the dataset content, not to provide security. Security
comes from the BLAKE3 calls that bookend it (seed derivation, mix
expansion, and the final hash), which is exactly the same division of
labor Ethash makes with Keccak.

## Step 4 — Validation

```
valid(final_hash, target) ⟺ interpret(final_hash) as big-endian <= target
```

`target` is derived from the current difficulty as `MAX_TARGET /
difficulty` (see `docs/ARCHITECTURE.md` for why it's not Bitcoin-style
compact bits).

## Node-side verification

Every node keeps the current epoch's full dataset in RAM (same "full
verification" approach as v1) to verify a received block's PoW — no
scratchpad or heavy per-hash compute involved this time, so verification
is now dramatically cheaper too: 64 dataset touches and a couple of BLAKE3
calls per verification, down from hundreds of thousands.

## What is NOT done yet (future work, not to be presented as delivered)

- **Real-hardware GPU throughput validated** (~13M H/s on an RTX 4070
  Super) but not yet tuned further — occupancy, work-group sizing, and
  larger batch sizes haven't been explored past the first working design.
- **CUDA-specific kernel**: only an OpenCL kernel exists; it runs on
  NVIDIA hardware via NVIDIA's OpenCL implementation, but a native CUDA
  path (or AMD-specific tuning) hasn't been written or benchmarked.
- **"Useful compute" layer** discussed during design (redirecting part of
  the mining work toward reusable computation): research-grade, requires a
  cheap verification mechanism for the "useful" work so it doesn't open a
  vulnerability. Not implemented — the interface is planned but empty.
- **Independent cryptographic audit** — a prerequisite for any launch with
  real value at stake.
