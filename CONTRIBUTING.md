# Contribuer à Ondris

Projet testnet en développement actif — l'architecture peut encore changer.

## Avant de proposer une modification

- `cargo build --workspace` et `cargo test --workspace` doivent passer.
- `cargo fmt --all` et `cargo clippy --workspace -- -D warnings` sont
  attendus propres.
- Toute modification de `ondris-pow` (l'algorithme lui-même) doit être
  discutée dans une issue avant la PR : c'est la partie la plus sensible du
  projet et elle nécessitera un audit avant tout lancement réel.

## Zones qui ont particulièrement besoin d'aide

Voir la liste des limitations connues dans
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — gestion des forks, mineur
GPU, mode de vérification "light client", découverte de pairs.
