# Ondris

A GPU-friendly, ASIC-resistant Proof-of-Work blockchain, with a reference
CPU miner and a command-line wallet.

## Mainnet launch: July 25, 2026

This is a target launch date, not a guarantee of a finished audit. As of
this writing, the items listed under "Known limitations" below
(independent cryptographic audit, GPU miner) are **not** complete. Anyone
mining, holding, or building on Ondris before and around the mainnet date
should treat it as early-stage, unaudited software and weigh that risk
accordingly.

## Status: experimental testnet, unaudited

**Do not use with real value.** The Proof-of-Work algorithm (`OndrisHash`,
see [docs/ALGORITHM.md](docs/ALGORITHM.md)) has not been reviewed by
independent cryptographers. The P2P transport is unencrypted. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full list of known
limitations and remaining work before a serious mainnet launch.

## What exists today

- `ondris-pow` — the OndrisHash algorithm (memory-hard, per-epoch dataset, GPU-friendly).
- `ondris-core` — blockchain types (block, transaction, account), validation, difficulty, genesis.
- `ondris-network` — basic TCP P2P gossip.
- `ondris-node` — full daemon: chain + network + HTTP RPC API.
- `ondris-miner` — reference CPU miner (multi-threaded).
- `ondris-wallet` — CLI wallet with encrypted keystore (Argon2 + AES-256-GCM).

What **does not exist yet**: a GPU miner (OpenCL/CUDA), the "useful compute"
layer discussed during design, an independent cryptographic audit.

## Known limitations (see docs/ARCHITECTURE.md for details)

- P2P transport is unencrypted, no peer discovery (static seed list only).
- No independent cryptographic audit of OndrisHash.
- Minimal mempool (transactions displaced by a reorg are re-queued automatically; a stale, never-submitted work template still drops its transactions).
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
