# Architecture

## Overview

```
┌─────────────┐      HTTP JSON       ┌──────────────┐
│ ondris-wallet│ ───────────────────▶│              │
└─────────────┘                      │              │
                                      │  ondris-node │◀──── TCP gossip ────▶ other nodes
┌─────────────┐      HTTP JSON       │  (chain +    │
│ ondris-miner │ ───────────────────▶│   network +  │
└─────────────┘                      │   RPC)       │
                                      └──────┬───────┘
                                             │ sled (embedded)
                                             ▼
                                        local disk
```

Crates:

- **ondris-primitives** — `Hash256`, `Address`, `KeyPair`/`PublicKey`/`Signature` (Ed25519). No dependency on the rest of the project.
- **ondris-pow** — the OndrisHash algorithm. Depends only on `ondris-primitives`.
- **ondris-core** — `BlockHeader`, `Transaction`, `Block`, `ChainState` (sled persistence), `Chain` (validation + application), difficulty, genesis, shared RPC DTOs.
- **ondris-network** — TCP P2P gossip, only aware of `ondris-core` types for messages.
- **ondris-node** — binary: wires up chain + network + HTTP server (axum).
- **ondris-miner** — binary: RPC client that fetches work, mines locally (CPU, multi-threaded), submits the found block.
- **ondris-miner-gpu** — binary: same RPC client role, but mining runs as an OpenCL kernel. See "The GPU miner" below for how it's validated.
- **ondris-wallet** — binary: encrypted keystore + RPC client for balance/sending transactions.

## Why an account model instead of a UTXO model

Simpler to reason about and to implement correctly in the time available
(a balance + a nonce per address, like Ethereum), at the cost of
transaction validation being slightly less naturally parallelizable than a
UTXO model. For a testnet, this trade-off is the right one.

## Why difficulty isn't stored as Bitcoin-style "compact bits"

Bitcoin's nBits format (32-bit exponent + mantissa) has tricky edge cases
(sign bit, rounding) that are a classic source of bugs when re-implemented
by hand. Ondris stores difficulty as a plain `u64` integer and computes the
target via `MAX_TARGET / difficulty` (256-bit division by a u64,
implemented directly). This is strictly equivalent in expressiveness for
our needs, with an implementation that's simpler to audit.

## How the miner regenerates the dataset without downloading it

The PoW dataset (tens of MB) is never transferred over the network.
`GET /work` returns the hash of the epoch boundary block
(`epoch_boundary_hash`); the miner locally computes the epoch seed
(`ondris_pow::epoch_seed`) and regenerates the dataset itself — exactly
like an Ethash miner regenerates its DAG from a lightweight seed. Every
node does the same to verify a received block.

## Fork handling and reorgs

`Chain::submit_block` accepts blocks that don't directly extend the
current tip. Every stored block (canonical or not) carries its own
cumulative chain work (`sum of block_work(difficulty)` back to genesis,
tracked in the `chainwork` sled tree); a new block only becomes the tip if
its cumulative work is strictly greater than the current tip's. When it
is, `Chain::reconsider_tip` walks both chains back to their common
ancestor, **simulates** the whole undo-then-reapply sequence against an
in-memory account overlay first, and only writes to `sled` if every
transaction on the winning branch actually checks out — a bad or
conflicting branch can never leave the database half-updated. Transactions
that were on the losing branch and aren't also on the winning one are
handed back to the node so it can return them to its mempool.

Blocks whose parent hasn't been seen yet come back as `SubmitOutcome::Orphan`
instead of an error; `ondris-node` buffers them and asks peers for the
missing parent (`Message::GetBlock` / `BlockResponse` in `ondris-network`),
retrying the buffered block (and anything buffered on top of it) once the
parent arrives.

Known simplification: `dataset_for_height` resolves an epoch's dataset via
the **canonical** height index, even when validating a side-branch block.
This is correct as long as competing branches don't diverge before an
epoch boundary (2,048 blocks) — true for the short races (two miners
finding a block seconds apart) this fork-choice rule is meant to resolve.
A branch that diverges earlier and still ends up winning would need
per-branch epoch tracking, which isn't implemented.

## The GPU miner

`ondris-miner-gpu`'s kernel (`crates/ondris-miner-gpu/src/kernel.cl`)
reimplements BLAKE3 from scratch in OpenCL C, since the `blake3` crate
doesn't run on a GPU. Porting a cryptographic primitive to a new language
by hand is exactly the kind of change where a transcription bug can look
fine and just be wrong — so before any of it touches OpenCL, the same
logic is built and validated in Rust first, where it's fast to iterate
and easy to compare against the real, audited crate:

1. `blake3_ref.rs` — BLAKE3 (compression function, single-chunk hashing,
   and the XOF/extendable-output mode used to expand the epoch seed into
   the dataset and the per-hash seed into the mix buffer) reimplemented
   from scratch, tested byte-for-byte against the real `blake3` crate
   across empty input, every chunk-count boundary from 1 to 16 (exact
   powers of two included — that's the specific case an earlier version
   of this function got wrong: the root flag never got applied when the
   chunk count collapsed the merge stack to one entry on its own, since
   nothing else was allowed to know that particular merge was the final
   one), randomized fuzzing, and the XOF path across various output
   lengths and seeds.
2. `ondris_hash_ref.rs` — the full FNV-mixing algorithm, built only from
   `blake3_ref` above, tested against `ondris_pow::ondris_hash` itself —
   including at the real default sizes (64 MiB dataset, 64 accesses).

Only once both passed did `kernel.cl` get written, as a mechanical
translation of the same logic. The kernel is then checked the same way,
on real hardware: `ondris-miner-gpu self-test` runs a debug-only kernel
(`ondris_hash_debug`) that returns a raw digest instead of just a target
comparison, and compares it against `ondris_pow::ondris_hash` at both
tiny and full-production sizes. The actual mining loop (`mine_block_gpu`
in `main.rs`) goes a step further and never trusts a GPU-reported hit on
its own either — every nonce the kernel flags gets re-hashed on the CPU
with the real reference function before it's ever submitted to a node.

Current status: correctness-validated this way on an NVIDIA RTX 4070
Super, and now genuinely GPU-scale on the same hardware: **~13 million
H/s** (`benchmark --batch-size 262144 --batches 15`), against a 4-thread
CPU miner's ~137 H/s — a ~95,000x gap in the intended direction. This
result only exists because of an algorithm redesign, not kernel tuning:
the original algorithm (see `docs/ALGORITHM.md`'s revision history) used
a CryptoNight/RandomX-style scratchpad mixed over hundreds of thousands
of sequential BLAKE3 calls per hash, which benchmarked at ~75 H/s on this
same GPU — slower than the CPU miner, because that workload is
compute-bound, and compute-bound workloads don't play to a GPU's actual
strength. Two kernel-level optimizations were tried against that original
algorithm first (removing an unnecessary scratchpad copy, replacing a
pointer-taking helper function with a macro) and neither changed
throughput at all, which is what motivated diagnosing the algorithm
itself rather than continuing to tune the kernel. The current v2 design
replaces the scratchpad with an Ethash-style dataset (a small, fixed
number of pseudo-random reads per hash, cheap FNV mixing between them),
which is memory-bandwidth-bound instead — the thing a GPU is actually
good at.

Not yet done: further occupancy/work-group tuning past this first working
design, and a native CUDA path (the current kernel runs on NVIDIA
hardware via NVIDIA's OpenCL implementation, not CUDA directly).

## Known limitations (future work, not done yet)

- **GPU miner further tuning**: correctness-validated and already
  GPU-scale (~13M H/s on an RTX 4070 Super, see "The GPU miner" above),
  but occupancy/work-group sizing and a native CUDA path haven't been
  explored past the first working design.
- **Minimal mempool**: `GET /work` drains the mempool on every call; if the
  resulting block is never submitted (miner crashes, restarts...), the
  transactions it contained are lost until the wallet resends them.
  Transactions displaced by a reorg *are* automatically re-queued (see
  above), but there's still no persistent, re-broadcast-aware mempool.
- **Unencrypted, unauthenticated P2P transport**: fine for a closed
  testnet, not for a public network with real value at stake.
- **No peer discovery (DHT)**: static seed node list provided in config;
  orphan resolution broadcasts `GetBlock` to every connected peer rather
  than targeting whoever is most likely to have it.
- **"Full" PoW verification only**: every node keeps the full dataset for
  the current epoch in RAM. A "light client" mode (on-the-fly regeneration
  of only the needed indices from the cache) is not implemented.
- **"Useful compute" layer** discussed during design: not implemented,
  research-grade.
- **No independent cryptographic audit.**
