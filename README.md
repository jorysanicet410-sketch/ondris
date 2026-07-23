# Ondris

A GPU-friendly, ASIC-resistant Proof-of-Work blockchain, with a reference
CPU miner and a command-line wallet.

## Mainnet launch: July 25, 2026

This is a target launch date, not a guarantee of a finished audit. As of
this writing, independent cryptographic audit listed under "Known
limitations" below is **not** complete. Anyone mining, holding, or
building on Ondris before and around the mainnet date should treat it as
early-stage, unaudited software and weigh that risk accordingly.

## Status: experimental testnet, unaudited

**Do not use with real value.** The Proof-of-Work algorithm (`OndrisHash`,
see [docs/ALGORITHM.md](docs/ALGORITHM.md)) has not been reviewed by
independent cryptographers. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full list of known
limitations and remaining work before a serious mainnet launch.

## What exists today

- `ondris-pow` — the OndrisHash algorithm (memory-hard, per-epoch dataset, GPU-friendly).
- `ondris-core` — blockchain types (block, transaction, account), validation, difficulty, genesis.
- `ondris-network` — TCP P2P gossip, wrapped in a Noise_XX-encrypted, mutually-authenticated transport (see docs/ARCHITECTURE.md) — no peer discovery yet, static seed list only.
- `ondris-node` — full daemon: chain + network + HTTP RPC API.
- `ondris-miner` — reference CPU miner (multi-threaded).
- `ondris-miner-gpu` — OpenCL GPU miner. Its kernel is a mechanical translation of a Rust reference chain (BLAKE3 → the FNV dataset-mixing algorithm) that's unit-tested against the real `blake3` crate first, then checked bit-for-bit against `ondris_pow::ondris_hash` on real hardware via `ondris-miner-gpu self-test` — run that before mining on any new GPU/driver. Correctness-validated on an RTX 4070 Super at ~16,600,000 H/s (~98% of its measured ceiling on that GPU) — see docs/ARCHITECTURE.md for how that compares to the CPU miner, the algorithm redesign that got it there, and why batch size matters as much as it does.
- `ondris-wallet` — CLI wallet with encrypted keystore (Argon2 + AES-256-GCM).

What **does not exist yet**: the "useful compute" layer discussed during
design, an independent cryptographic audit.

## Known limitations (see docs/ARCHITECTURE.md for details)

- P2P transport is encrypted and mutually authenticated (Noise_XX), but
  there's still no peer discovery — a static seed list only.
- No independent cryptographic audit of OndrisHash.
- Mempool is persistent (survives a node restart) and transactions displaced by a reorg or a never-submitted work template are both automatically re-queued — but there's still no peer reputation/prioritization beyond FIFO.
- "Full" PoW verification only (every node holds the entire epoch dataset in RAM); no light-client mode yet.
- Fork/reorg handling assumes competing branches don't diverge across an epoch boundary (2,048 blocks) — see docs/ARCHITECTURE.md.

## Requirements

- [Rust](https://rustup.rs/) (2021 edition, tested with 1.96+)

## Build

```bash
cargo build --release --workspace
```

## Run a testnet node

```bash
cargo run --release --bin ondris-node -- \
  --data-dir ./ondris-data \
  --genesis ./config/testnet-genesis.json \
  --p2p-addr 0.0.0.0:30303 \
  --rpc-addr 127.0.0.1:8080
```

To join an existing testnet, add `--peer <ip>:30303` (repeatable).

## Create a wallet

```bash
cargo run --release --bin ondris-wallet -- new --out my-wallet.json
```

## Mine

```bash
cargo run --release --bin ondris-miner -- \
  --node http://127.0.0.1:8080 \
  --address <address-shown-by-the-wallet> \
  --threads 4
```

## Mine on a GPU (OpenCL)

Requires an OpenCL-capable GPU (NVIDIA, AMD) and its drivers. Run the
self-test first, on any new GPU or driver version, before trusting it to
mine for real:

```bash
cargo run --release --bin ondris-miner-gpu -- self-test
```

That checks the kernel's output against the CPU reference implementation
at both tiny and full-size parameters — it should print `ALL CHECKS
PASSED`. Then:

```bash
cargo run --release --bin ondris-miner-gpu -- mine \
  --node http://127.0.0.1:8080 \
  --address <address-shown-by-the-wallet>
```

`--batch-size` (nonces tried per kernel launch) defaults to 1,048,576.
The per-epoch dataset (the only large buffer this algorithm needs) is
uploaded once and shared read-only across every work-item, so batch size
isn't bounded by a per-nonce private allocation the way the original
scratchpad-based design was — it's bounded by how long you're willing to
wait between result checks. Too small a batch leaves real throughput
unused (a much smaller batch measured barely 70% of this GPU's ceiling,
not because of per-batch overhead but because too few work-items in
flight can't hide this kernel's memory latency across all compute units)
— run the `benchmark` subcommand at a few sizes to find where it plateaus
on your own GPU.

## Send a transaction

```bash
cargo run --release --bin ondris-wallet -- send \
  --wallet my-wallet.json \
  --to <recipient-address> \
  --amount 100000000 \
  --node http://127.0.0.1:8080
```

(1 ONDR = 100,000,000 smallest units, like satoshis for Bitcoin.)

## Tests

```bash
cargo test --workspace
```

## Documentation

- [docs/ALGORITHM.md](docs/ALGORITHM.md) — full spec of the PoW algorithm.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — architecture, technical choices, known limitations.
- [docs/WHITEPAPER.md](docs/WHITEPAPER.md) — project overview.

## License

MIT, see [LICENSE](LICENSE).
