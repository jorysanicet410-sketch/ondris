# OndrisHash — Algorithme de Proof-of-Work

## Statut

**Non audité.** Cette spec et son implémentation de référence n'ont pas encore été
revues par des cryptographes indépendants. Ne pas lui faire confiance avec de la
valeur réelle avant un audit externe. OndrisHash ne réinvente aucune primitive
cryptographique : elle combine BLAKE3 (fonction de hachage auditée, standardisée)
et un générateur pseudo-aléatoire déterministe dans une architecture originale
de type "memory-hard + accès mémoire dépendant des données", inspirée des
familles Ethash (dataset par époque) et CryptoNight/RandomX (scratchpad mixing).
Ce qui est nouveau ici, c'est l'**architecture et le paramétrage**, pas les
briques cryptographiques sous-jacentes.

## Objectifs de conception

1. **GPU-friendly** : accès mémoire massivement parallèles et uniformes,
   ce qui correspond exactement à la force d'un GPU (bande passante mémoire
   élevée, milliers de threads).
2. **ASIC-résistant** : chaque hash nécessite un accès aléatoire à un dataset
   de plusieurs centaines de Mo à quelques Go. Un ASIC dédié devrait embarquer
   la même quantité de RAM rapide qu'un GPU, ce qui annule son avantage de coût/consommation.
3. **CPU-résistant modérément** : un CPU peut techniquement calculer l'algorithme
   (nécessaire pour la vérification par les nodes), mais son débit est très
   inférieur à un GPU à cause de la bande passante mémoire plus faible et du
   nombre de threads limité.

## Paramètres

| Constante | Valeur testnet | Description |
|---|---|---|
| `EPOCH_LENGTH` | 2048 blocs | Fréquence de régénération du dataset |
| `CACHE_SIZE` | 16 Mio | Graine compacte dérivée du seed d'époque |
| `DATASET_SIZE` | 64 Mio (testnet/dev) / 2-4 Gio (mainnet cible) | Dataset complet utilisé pour le mixing |
| `SCRATCHPAD_SIZE` | 2 Mio | Mémoire de travail par tentative de hash |
| `MIX_ROUNDS` | 8 | Nombre de tours de mixing dépendant des données |

Les tailles testnet sont volontairement réduites pour que le développement et les
tests tournent vite sur du matériel modeste (y compris CPU). Les valeurs mainnet
seront revues avec l'auditeur avant tout lancement réel.

## Étape 1 — Seed d'époque

```
epoch(height) = height / EPOCH_LENGTH
epoch_seed(0) = BLAKE3("ONDRIS_GENESIS_EPOCH")
epoch_seed(e) = BLAKE3(hash_of_block_at(e * EPOCH_LENGTH))   pour e > 0
```

Le seed d'époque dépend du contenu réel de la chaîne (hash d'un bloc miné),
ce qui empêche de précalculer les datasets futurs à l'avance.

## Étape 2 — Cache et dataset

```
cache = BLAKE3_XOF(epoch_seed, output_len = CACHE_SIZE)

dataset[i] pour i in [0, DATASET_SIZE / 64) :
    item = cache[(i * 64) % CACHE_SIZE .. +64]
    répéter 2 fois:
        item = BLAKE3(item || i.to_le_bytes())
    dataset[i*64 .. +64] = item
```

Le cache est petit et rapide à générer (ou vérifier en mode "light client").
Le dataset complet est ce que les mineurs génèrent une fois par époque et
gardent en mémoire (VRAM) pour tout miner l'époque.

## Étape 3 — Hash d'un essai (header + nonce)

```
input   = header_bytes || nonce.to_le_bytes()
seed    = BLAKE3(input)                       // 32 octets
prng    = Xoshiro256** seedé par `seed`
scratchpad = [0u8; SCRATCHPAD_SIZE]

// Initialisation : on remplit le scratchpad avec des tranches du dataset
// choisies pseudo-aléatoirement (c'est ici que la "largeur mémoire" est requise)
pour chaque bloc de 64 octets du scratchpad:
    idx = prng.next_u64() % (DATASET_SIZE / 64)
    scratchpad[bloc] = dataset[idx*64 .. +64] XOR seed_étendu(bloc)

// Mixing : MIX_ROUNDS tours de mélange dépendant des données déjà écrites
pour round in 0..MIX_ROUNDS:
    pour chaque bloc de 64 octets du scratchpad à la position p:
        idx_dep = prng.next_u64() % (SCRATCHPAD_SIZE / 64)   // dépend de l'état courant
        scratchpad[p] = BLAKE3(scratchpad[p] || scratchpad[idx_dep])[..64]

final_hash = BLAKE3(scratchpad)   // 32 octets
```

L'étape de mixing lit et écrit le scratchpad de façon **dépendante des données
déjà calculées** (comme CryptoNight/RandomX) : impossible de paralléliser tous
les rounds à l'avance, ce qui limite l'avantage d'un circuit figé sans mémoire
suffisante pour tenir l'état intermédiaire.

## Étape 4 — Validation

```
valide(final_hash, target) ⟺ interpréter(final_hash) en big-endian <= target
```

`target` est dérivé de la difficulté courante exactement comme le `nBits`
de Bitcoin (format compact 32 bits : exposant + mantisse).

## Vérification par un node (pas besoin de miner)

Un node qui reçoit un bloc doit pouvoir vérifier le PoW sans avoir miné.
Deux options, à trancher avant l'implémentation finale :

- **Vérification "full"** : le node maintient aussi le dataset complet de
  l'époque courante (comme Ethash côté node complet) — coûteux en RAM mais
  simple.
- **Vérification "light"** : régénérer à la volée, pour les quelques indices
  réellement accédés durant le calcul, les valeurs de dataset nécessaires à
  partir du `cache` seul (comme Ethash côté client léger) — plus lent par
  hash mais RAM négligeable.

Pour la première implémentation (testnet), on choisit la vérification **full**
pour rester simple ; le mode "light" est documenté comme travail futur.

## Ce qui n'est PAS encore fait (travail futur, à ne pas présenter comme livré)

- **Kernel GPU (OpenCL/CUDA)** : cette spec définit les règles de consensus
  via une implémentation de référence CPU. Un mineur GPU performant est un
  travail séparé qui portera cette même logique sur GPU.
- **Couche "calcul utile"** évoquée dans les discussions de conception
  (rediriger une partie du travail de minage vers du calcul réutilisable) :
  research-grade, nécessite un mécanisme de vérification bon marché du
  travail "utile" pour ne pas ouvrir de faille (un node ne doit jamais avoir
  à refaire le calcul utile en entier pour vérifier un bloc). Non implémenté
  dans cette première version — interface prévue mais vide.
- **Audit cryptographique indépendant** — condition préalable à tout lancement
  avec de la valeur réelle en jeu.
