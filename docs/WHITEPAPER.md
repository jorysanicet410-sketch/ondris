# Ondris — Technical Overview

*Technical document, not an investment prospectus. Ondris is, at this
stage, neither audited nor launched on mainnet. Nothing in this document
constitutes financial advice or a promise of future value.*

## Motivation

Most Proof-of-Work cryptocurrencies converge, over time, toward mining
dominated by dedicated ASICs: mining stops being accessible to anyone with
a consumer GPU. Ondris aims for an algorithm that stays GPU-friendly over
the long run by relying on a structural constraint (massive access to fast
RAM) rather than hoping nobody builds the corresponding ASIC.

## Technical approach

OndrisHash combines, in an original architecture, cryptographic primitives
that are already audited (BLAKE3) rather than introducing a new, unproven
hash primitive:

- a **dataset regenerated per epoch** (like Ethash), derived from the
  chain's actual content — prevents precomputation;
- a **scratchpad mixed in a data-dependent way** on data already written
  (like CryptoNight/RandomX) — prevents trivial parallelization without
  enough memory to hold the intermediate state.

Full details are in [ALGORITHM.md](ALGORITHM.md), including its current
limitations and what remains before an audit.

## Project status

| Component | Status |
|---|---|
| OndrisHash algorithm (CPU reference implementation) | Functional, unaudited |
| Node (chain + P2P network + RPC) | Functional, testnet only |
| CLI wallet | Functional |
| Reference CPU miner | Functional |
| GPU miner (OpenCL/CUDA) | Not started |
| Fork/reorg handling | Functional (see docs/ARCHITECTURE.md for known simplifications) |
| Independent cryptographic audit | Not done |
| "Useful compute" layer | Not implemented (research-grade) |

## Token economics (testnet parameters, to be revisited before mainnet)

- Decreasing emission via halving (like Bitcoin), every 210,000 blocks.
- Initial block reward: 50 ONDR.
- Target block time: 30 seconds.
- Difficulty retarget every 60 blocks.
- No premine by default in the provided testnet config
  (`config/testnet-genesis.json`) — any foundation allocation will need to
  be explicitly decided, documented, and made public before any real
  launch.

## What this document does not do

It does not claim the algorithm is safe in the absence of an independent
audit. It makes no promise about the future value of any token. Any
decision to mine or acquire an Ondris token, if a real network is ever
launched, should be preceded by an independent check of the code's state
at that time — not of this document.

## Next steps

1. Public testnet, open to volunteer miners.
2. Fixing bugs surfaced by the testnet.
3. Chain fork/reorganization handling.
4. Independent cryptographic audit of OndrisHash.
5. Reference GPU miner (OpenCL/CUDA).
6. Legal counsel on regulatory classification before any solicitation of
   investors.
