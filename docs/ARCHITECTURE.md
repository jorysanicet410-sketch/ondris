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

## P2P transport encryption

Every connection between two nodes (`ondris-network/src/noise.rs`) is
wrapped in a Noise_XX handshake before a single application byte is sent,
using the `snow` crate — a standard Noise Protocol Framework
implementation, not a bespoke construction, for the same reason this
project uses BLAKE3 instead of inventing a hash function: don't roll your
own cryptography. Noise_XX is the same family of construction WireGuard
and the Lightning Network use for their transport security, and is the
right pattern for a permissionless P2P network specifically because it
doesn't require either side to know the other's public key in advance —
a node just needs to prove, cryptographically, that whoever it's talking
to for the rest of the connection is the same party it shook hands with,
not that this party is on any particular allow-list (there is no
allow-list; anyone can connect with a freshly generated identity).

Each node has a persistent X25519 keypair (`<data-dir>/node_identity.key`,
generated on first startup) — separate from the Ed25519 keys used for
wallet/transaction signing, since transport identity and spending
authority are different concerns on different curves. After a successful
handshake, both sides have an encrypted, integrity-protected channel
(ChaCha20-Poly1305) and each has cryptographic proof of the other's
static public key, logged as a `PeerId` alongside the connection's
`SocketAddr`.

Application messages can be larger than Noise's 65535-byte per-message
cap (a full block, in particular), so `EncryptedWriter`/`EncryptedReader`
chunk a logical write across as many Noise frames as needed and
reassemble them on the other side — validated with a dedicated test that
forces chunking with a >200KB payload, plus a regression test for a bug
caught in a live two-node smoke test (not just unit tests): a single
small message written as one Noise frame decrypts to more bytes than a
`read_exact(4)` call for its length prefix asks for, and those leftover
bytes have to survive into the *next* read call rather than being
dropped — exactly what `EncryptedReader`'s internal buffer exists to fix.
A third test manually flips a bit in a real ciphertext and confirms
decryption fails outright rather than silently returning garbage.

What this does **not** change: there's still no peer discovery (a static
seed list only) and no reputation/banning system — Noise authenticates
*that you're still talking to the same peer you handshook with*, not
*that this peer is trustworthy*.

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
Super, and now genuinely GPU-scale on the same hardware: **~16.6 million
H/s** (`benchmark --batch-size 1048576 --batches 20`), about 98% of this
kernel's measured throughput ceiling on that GPU (~16.8M H/s, seen at
batch sizes from ~4M up to 16M with no further gain). The apples-to-apples
comparison is against a 4-thread CPU miner running the *same* (v2)
algorithm on the same machine, which does ~750,000 H/s — a **~22x GPU
advantage**. This result only exists because of an algorithm redesign,
not kernel tuning: the original algorithm (see `docs/ALGORITHM.md`'s
revision history) used a CryptoNight/RandomX-style scratchpad mixed over
hundreds of thousands of sequential BLAKE3 calls per hash, which
benchmarked at ~75 H/s on this same GPU — slower than a 4-thread CPU miner
running that same v1 algorithm (~137 H/s), because that workload is
compute-bound, and compute-bound workloads don't play to a GPU's actual
strength. Two kernel-level optimizations were tried against that original
algorithm first (removing an unnecessary scratchpad copy, replacing a
pointer-taking helper function with a macro) and neither changed
throughput at all, which is what motivated diagnosing the algorithm
itself rather than continuing to tune the kernel. The current v2 design
replaces the scratchpad with an Ethash-style dataset (a small, fixed
number of pseudo-random reads per hash, cheap FNV mixing between them),
which is memory-bandwidth-bound instead — the thing a GPU is actually
good at, and which also made the algorithm far cheaper to compute on a
CPU (64 dataset touches vs. 500,000+ sequential hashes), so v2's CPU
miner is itself roughly 5,500x faster than v1's was.

**Batch size matters a lot more than it first looked.** The `mine`
subcommand's original default (65,536 nonces per kernel launch) only
reached ~11.9M H/s — leaving ~30% of this GPU's real throughput unused,
not because of host-side overhead (a first attempt cached the per-batch
header/target buffer uploads, which turned out to change throughput by
under 1%) but because too few work-items in flight can't hide this
kernel's per-thread memory latency across all 56 compute units. Batch
size has no upper bound tied to VRAM here (there's no per-thread
scratchpad to allocate, unlike the old design), so it was raised until
throughput stopped improving — measured, not guessed, exactly like the
v1→v2 algorithm diagnosis. The new default is 1,048,576: near-ceiling
throughput while still checking in roughly every 60ms, versus 250ms–1s
checks at 4M–16M for no meaningful additional throughput. Run `benchmark`
yourself to find the right value for a different GPU.

Not yet done: further occupancy/work-group tuning beyond batch size, and
a native CUDA path (the current kernel runs on NVIDIA hardware via
NVIDIA's OpenCL implementation, not CUDA directly).

## Mempool persistence

Pending transactions live in a `sled` tree (`ChainState::mempool_*`),
keyed by transaction hash — not an in-memory `Vec`, so a node restart no
longer loses them. This replaced an earlier design where `GET /work`
destructively drained the whole mempool on every call: if the resulting
block was never submitted (miner crash, a stale template beaten by a
faster peer), those transactions were gone until the wallet resent them.
`GET /work` now only *reads* a snapshot (`mempool_all`); a transaction is
only actually removed once the block containing it is truly `Accepted`
onto the canonical chain (`handle_outcome` in `ondris-node`), which reuses
the same `requeue` mechanism a reorg already relies on to put displaced
transactions back — a block getting reorged out and a work template never
getting submitted are the same kind of event from the mempool's point of
view (a transaction that didn't end up canonical after all), so both are
handled by the same insert/remove pair rather than two separate code
paths. A background task also rebroadcasts whatever's still pending every
30 seconds, so a transaction that only reached one node still eventually
reaches the rest of the network.

Verified with a persistence test (insert into a mempool, drop and reopen
`ChainState` at the same path, confirm the transaction is still there)
and a live end-to-end run: submit a transaction, confirm two consecutive
`GET /work` calls both still show it (not drained), kill and restart the
node process entirely, confirm it's *still* pending after the restart,
then mine a block and confirm the transaction gets included and the
mempool clears.

## Known limitations (future work, not done yet)

- **GPU miner further tuning**: correctness-validated and batch-size-tuned
  to ~98% of its measured ceiling (~16.6M H/s on an RTX 4070 Super, see
  "The GPU miner" above), but occupancy/work-group sizing beyond batch
  size and a native CUDA path haven't been explored.
- **No peer discovery (DHT)**: static seed node list provided in config;
  orphan resolution broadcasts `GetBlock` to every connected peer rather
  than targeting whoever is most likely to have it. The transport itself
  is now encrypted and mutually authenticated (see "P2P transport
  encryption" above) — what's missing is *finding* peers, not securing
  the link to ones you already know about.
- **"Full" PoW verification only**: every node keeps the full dataset for
  the current epoch in RAM. A "light client" mode (on-the-fly regeneration
  of only the needed indices from the cache) is not implemented.
- **"Useful compute" layer** discussed during design: not implemented,
  research-grade.
- **No independent cryptographic audit.**
