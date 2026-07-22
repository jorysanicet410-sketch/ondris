# Ondris — présentation technique

*Document technique, pas un prospectus d'investissement. Ondris n'est, à ce
stade, ni auditée, ni lancée en mainnet. Rien dans ce document ne constitue
un conseil financier ni une promesse de valeur future.*

## Motivation

La plupart des cryptomonnaies Proof-of-Work convergent, avec le temps, vers
un minage dominé par des ASIC dédiés : le minage cesse d'être accessible à
quiconque possède un GPU grand public. Ondris vise un algorithme qui reste
GPU-friendly durablement en s'appuyant sur une contrainte structurelle (accès
massif à de la mémoire vive rapide) plutôt que sur l'espoir que personne ne
construise l'ASIC correspondant.

## Approche technique

OndrisHash combine, dans une architecture originale, des primitives
cryptographiques déjà auditées (BLAKE3) plutôt que d'introduire une nouvelle
primitive de hachage non éprouvée :

- un **dataset régénéré par époque** (comme Ethash), dérivé du contenu réel
  de la chaîne — empêche le précalcul ;
- un **scratchpad mélangé de façon dépendante des données** déjà écrites
  (comme CryptoNight/RandomX) — empêche la parallélisation triviale sans
  mémoire suffisante pour tenir l'état intermédiaire.

Le détail complet est dans [ALGORITHM.md](ALGORITHM.md), y compris ses
limites actuelles et ce qui reste à faire avant un audit.

## État du projet

| Composant | État |
|---|---|
| Algorithme OndrisHash (implémentation de référence CPU) | Fonctionnel, non audité |
| Node (chaîne + réseau P2P + RPC) | Fonctionnel, testnet uniquement |
| Wallet CLI | Fonctionnel |
| Mineur CPU de référence | Fonctionnel |
| Mineur GPU (OpenCL/CUDA) | Non commencé |
| Gestion des forks/réorganisations | Non implémentée |
| Audit cryptographique indépendant | Non réalisé |
| Couche "calcul utile" | Non implémentée (research-grade) |

## Économie du jeton (paramètres testnet, à revoir avant mainnet)

- Émission décroissante par halving (comme Bitcoin), tous les 210 000 blocs.
- Récompense de bloc initiale : 50 ONDR.
- Bloc cible : 30 secondes.
- Réajustement de difficulté toutes les 60 blocs.
- Pas de pré-mine par défaut dans la config testnet fournie
  (`config/testnet-genesis.json`) — toute allocation de fondation devra être
  décidée explicitement, documentée, et rendue publique avant tout lancement
  réel.

## Ce que ce document ne fait pas

Il ne prétend pas que l'algorithme est sûr en l'absence d'audit indépendant.
Il ne fait aucune promesse sur la valeur future d'un éventuel jeton. Toute
décision de miner ou d'acquérir un jeton Ondris, si un réseau réel est un
jour lancé, devrait être précédée d'une vérification indépendante de l'état
du code à ce moment-là — pas de ce document.

## Prochaines étapes

1. Testnet public, ouvert à des mineurs volontaires.
2. Correction des bugs remontés par le testnet.
3. Gestion des forks/réorganisations de chaîne.
4. Audit cryptographique indépendant de OndrisHash.
5. Mineur GPU de référence (OpenCL/CUDA).
6. Conseil juridique sur la qualification réglementaire avant toute
   sollicitation d'investisseurs.
