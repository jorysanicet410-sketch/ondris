# Architecture

## Vue d'ensemble

```
┌─────────────┐      HTTP JSON       ┌──────────────┐
│ ondris-wallet│ ───────────────────▶│              │
└─────────────┘                      │              │
                                      │  ondris-node │◀──── TCP gossip ────▶ autres nodes
┌─────────────┐      HTTP JSON       │  (chain +    │
│ ondris-miner │ ───────────────────▶│   network +  │
└─────────────┘                      │   RPC)       │
                                      └──────┬───────┘
                                             │ sled (embarqué)
                                             ▼
                                        disque local
```

Crates :

- **ondris-primitives** — `Hash256`, `Address`, `KeyPair`/`PublicKey`/`Signature` (Ed25519). Aucune dépendance sur le reste du projet.
- **ondris-pow** — l'algorithme OndrisHash. Dépend uniquement de `ondris-primitives`.
- **ondris-core** — `BlockHeader`, `Transaction`, `Block`, `ChainState` (persistance sled), `Chain` (validation + application), difficulté, genesis, DTOs RPC partagés.
- **ondris-network** — gossip P2P TCP, ne connaît que les types de `ondris-core` pour les messages.
- **ondris-node** — binaire : assemble chain + network + serveur HTTP (axum).
- **ondris-miner** — binaire : client RPC qui récupère du travail, mine en local (CPU, multi-thread), soumet le bloc trouvé.
- **ondris-wallet** — binaire : keystore chiffré + client RPC pour solde/envoi de transaction.

## Pourquoi un modèle de compte plutôt qu'un modèle UTXO

Plus simple à raisonner et à implémenter correctement dans le temps
disponible (un solde + un nonce par adresse, comme Ethereum), au prix d'une
parallélisation de la validation des transactions un peu moins naturelle
qu'un modèle UTXO. Pour un testnet, ce compromis est le bon.

## Pourquoi la difficulté n'est pas au format "compact bits" façon Bitcoin

Le format nBits de Bitcoin (exposant + mantisse sur 32 bits) a des cas
limites délicats (bit de signe, arrondis) qui sont une source classique de
bugs si ré-implémentés à la main. Ondris stocke la difficulté comme un
simple entier `u64` et calcule la cible via `MAX_TARGET / difficulty`
(division 256 bits par un u64, implémentée directement). C'est strictement
équivalent en expressivité pour nos besoins, avec une implémentation plus
simple à auditer.

## Comment le mineur régénère le dataset sans le télécharger

Le dataset PoW (plusieurs dizaines de Mio) n'est jamais transféré sur le
réseau. `GET /work` renvoie le hash du bloc de bordure d'époque
(`epoch_boundary_hash`) ; le mineur calcule localement le seed d'époque
(`ondris_pow::epoch_seed`) et régénère le dataset lui-même — exactement
comme un mineur Ethash régénère son DAG à partir d'un seed léger. Chaque
node fait de même pour vérifier un bloc reçu.

## Limitations connues (travail futur, pas encore fait)

- **Pas de gestion de fork/réorganisation** : `Chain::submit_block`
  n'accepte que l'extension linéaire du tip courant. Si deux mineurs
  trouvent un bloc en même temps, un des deux sera simplement rejeté par le
  reste du réseau plutôt que de déclencher une vraie réorganisation vers la
  chaîne la plus lourde. Nécessaire avant tout testnet avec plusieurs
  mineurs actifs simultanément.
- **Mempool minimaliste** : `GET /work` vide le mempool à chaque appel ; si
  le bloc résultant n'est jamais soumis (mineur qui plante, redémarre...),
  les transactions qu'il contenait sont perdues et doivent être renvoyées
  par le wallet. Pas de re-file d'attente automatique.
- **Transport P2P en clair, sans authentification** : suffisant pour un
  testnet fermé, pas pour un réseau public avec de la valeur réelle.
- **Pas de découverte de pairs (DHT)** : liste de seed nodes statique fournie
  en config.
- **Vérification "full" du PoW seulement** : chaque node garde le dataset
  complet de l'époque courante en RAM. Un mode "light client" (régénération
  à la volée des seuls indices nécessaires depuis le cache) n'est pas
  implémenté.
- **Couche "calcul utile"** évoquée en conception : pas implémentée, research-grade.
- **Aucun audit cryptographique indépendant.**
