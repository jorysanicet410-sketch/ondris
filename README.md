# Ondris

Blockchain Proof-of-Work GPU-friendly et résistante aux ASIC, avec un
mineur de référence CPU et un wallet en ligne de commande.

## Statut : testnet expérimental, non audité

**Ne pas utiliser avec de la valeur réelle.** L'algorithme de Proof-of-Work
(`OndrisHash`, voir [docs/ALGORITHM.md](docs/ALGORITHM.md)) n'a pas été revu
par des cryptographes indépendants. Le node ne gère pas encore les
réorganisations de chaîne (forks). Le transport P2P n'est pas chiffré. Voir
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) pour la liste complète des
limitations connues et du travail restant avant un lancement mainnet sérieux.

## Ce qui existe aujourd'hui

- `ondris-pow` — l'algorithme OndrisHash (memory-hard, dataset par époque, GPU-friendly).
- `ondris-core` — types de blockchain (bloc, transaction, compte), validation, difficulté, genesis.
- `ondris-network` — gossip P2P basique sur TCP.
- `ondris-node` — daemon complet : chaîne + réseau + API RPC HTTP.
- `ondris-miner` — mineur CPU de référence (multi-thread).
- `ondris-wallet` — wallet CLI avec keystore chiffré (Argon2 + AES-256-GCM).

Ce qui **n'existe pas encore** : un mineur GPU (OpenCL/CUDA), la couche de
"calcul utile" évoquée en phase de conception, un audit cryptographique.

## Prérequis

- [Rust](https://rustup.rs/) (édition 2021, testé avec 1.96+)

## Build

```bash
cargo build --release --workspace
```

## Lancer un node testnet

```bash
cargo run --release --bin ondris-node -- \
  --data-dir ./ondris-data \
  --genesis ./config/testnet-genesis.json \
  --p2p-addr 0.0.0.0:30303 \
  --rpc-addr 127.0.0.1:8080
```

Pour rejoindre un testnet existant, ajoute `--peer <ip>:30303` (répétable).

## Créer un wallet

```bash
cargo run --release --bin ondris-wallet -- new --out mon-wallet.json
```

## Miner

```bash
cargo run --release --bin ondris-miner -- \
  --node http://127.0.0.1:8080 \
  --address <adresse-affichée-par-le-wallet> \
  --threads 4
```

## Envoyer une transaction

```bash
cargo run --release --bin ondris-wallet -- send \
  --wallet mon-wallet.json \
  --to <adresse-destinataire> \
  --amount 100000000 \
  --node http://127.0.0.1:8080
```

(1 ONDR = 100 000 000 plus petites unités, comme le satoshi pour Bitcoin.)

## Tests

```bash
cargo test --workspace
```

## Documentation

- [docs/ALGORITHM.md](docs/ALGORITHM.md) — spec complète de l'algorithme PoW.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — architecture, choix techniques, limitations connues.
- [docs/WHITEPAPER.md](docs/WHITEPAPER.md) — présentation du projet.

## Licence

MIT, voir [LICENSE](LICENSE).
