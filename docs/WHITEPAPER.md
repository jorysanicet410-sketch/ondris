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

OndrisHash builds on a cryptographic primitive that's already audited
(BLAKE3) rather than introducing a new, unproven hash primitive, combined
in an Ethash-style architecture — the same shape that has secured
Ethereum's mainnet for years:

- a **dataset regenerated per epoch**, derived from the chain's actual
  content — prevents precomputation;
- a **small, fixed number of pseudo-random reads into that dataset per
  hash attempt** (64), combined with a cheap, non-cryptographic FNV
  mix — dominates each hash attempt with real memory bandwidth rather
  than raw compute, which is what makes it play to a GPU's actual
  strength.

An earlier design instead mixed a scratchpad over hundreds of thousands
of sequential BLAKE3 calls per hash (CryptoNight/RandomX-style) — that
shape is a deliberate choice those algorithms make to favor CPUs and
starve GPUs, which is exactly backwards from this project's goal, and was
confirmed in practice before being replaced: see
[ALGORITHM.md](ALGORITHM.md)'s revision history for the measured numbers.

Full details are in [ALGORITHM.md](ALGORITHM.md), including its current
limitations and what remains before an audit.

## Project status

| Component | Status |
|---|---|
| OndrisHash algorithm (CPU reference implementation) | Functional, unaudited |
| Node (chain + P2P network + RPC) | Functional, testnet only |
| CLI wallet | Functional |
| Reference CPU miner | Functional (~750K H/s, 4 threads, reference hardware) |
| GPU miner (OpenCL) | Functional, correctness-validated on real hardware (RTX 4070 Super); ~12.9M H/s measured on the same machine, ~17x the CPU reference miner |
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

Done since the testnet opened: chain fork/reorganization handling, a
reference GPU miner (OpenCL, correctness-validated and GPU-scale), an
encrypted and mutually-authenticated P2P transport (Noise_XX), and a
persistent, re-broadcast-aware mempool — see docs/ARCHITECTURE.md for how
each of these works and what they don't cover yet. What's left:

1. Public testnet, open to volunteer miners.
2. Fixing bugs surfaced by the testnet.
3. Independent cryptographic audit of OndrisHash — the single largest
   remaining item, and a prerequisite for trusting the v2 algorithm with
   real value.
4. Real peer discovery (currently a static seed list) — the transport
   itself is encrypted and authenticated, so what's left here is finding
   peers, not securing the link to ones already known.
5. A light-client PoW verification mode (today every node holds the full
   epoch dataset in RAM).
6. Legal counsel on regulatory classification before any solicitation of
   investors.
