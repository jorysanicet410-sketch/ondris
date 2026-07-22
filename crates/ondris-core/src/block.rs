use crate::header::BlockHeader;
use crate::transaction::Transaction;
use ondris_primitives::Hash256;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

/// Racine de Merkle simple (arbre binaire, duplication du dernier élément
/// impair) sur les hashs de transaction.
pub fn merkle_root(hashes: &[Hash256]) -> Hash256 {
    if hashes.is_empty() {
        return Hash256::ZERO;
    }
    let mut level: Vec<Hash256> = hashes.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let combined = if pair.len() == 2 {
                let mut buf = Vec::with_capacity(64);
                buf.extend_from_slice(pair[0].as_bytes());
                buf.extend_from_slice(pair[1].as_bytes());
                Hash256::hash(&buf)
            } else {
                let mut buf = Vec::with_capacity(64);
                buf.extend_from_slice(pair[0].as_bytes());
                buf.extend_from_slice(pair[0].as_bytes());
                Hash256::hash(&buf)
            };
            next.push(combined);
        }
        level = next;
    }
    level[0]
}

impl Block {
    pub fn compute_tx_root(&self) -> Hash256 {
        let hashes: Vec<Hash256> = self.transactions.iter().map(|t| t.hash()).collect();
        merkle_root(&hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_merkle_root_is_zero() {
        assert_eq!(merkle_root(&[]), Hash256::ZERO);
    }

    #[test]
    fn single_leaf_root_equals_the_leaf_itself() {
        // Une seule feuille : la boucle de réduction ne s'exécute pas
        // (elle ne s'exécute que tant qu'il reste >1 élément), donc la
        // racine est directement ce hash-là, sans duplication.
        let h = Hash256::hash(b"tx1");
        assert_eq!(merkle_root(&[h]), h);
    }

    #[test]
    fn odd_number_of_leaves_duplicates_the_last_one() {
        let h1 = Hash256::hash(b"tx1");
        let h2 = Hash256::hash(b"tx2");
        let h3 = Hash256::hash(b"tx3");
        let root = merkle_root(&[h1, h2, h3]);

        let mut left = Vec::new();
        left.extend_from_slice(h1.as_bytes());
        left.extend_from_slice(h2.as_bytes());
        let left_hash = Hash256::hash(&left);

        let mut right = Vec::new();
        right.extend_from_slice(h3.as_bytes());
        right.extend_from_slice(h3.as_bytes());
        let right_hash = Hash256::hash(&right);

        let mut top = Vec::new();
        top.extend_from_slice(left_hash.as_bytes());
        top.extend_from_slice(right_hash.as_bytes());
        assert_eq!(root, Hash256::hash(&top));
    }
}
