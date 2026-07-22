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

## Known limitations (future work, not done yet)

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
