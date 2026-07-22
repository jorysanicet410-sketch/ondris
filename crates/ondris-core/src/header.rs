use ondris_pow::Dataset;
use ondris_primitives::{Address, Hash256};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockHeader {
    pub height: u64,
    pub prev_hash: Hash256,
    pub tx_root: Hash256,
    pub timestamp: u64,
    pub difficulty: u64,
    pub miner: Address,
    pub nonce: u64,
}

impl BlockHeader {
    /// Sérialisation canonique de l'en-tête SANS le nonce : c'est le
    /// `header_bytes` passé à `ondris_hash`, qui ajoute le nonce lui-même.
    pub fn bytes_for_pow(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96);
        buf.extend_from_slice(&self.height.to_le_bytes());
        buf.extend_from_slice(self.prev_hash.as_bytes());
        buf.extend_from_slice(self.tx_root.as_bytes());
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.difficulty.to_le_bytes());
        buf.extend_from_slice(&self.miner.0);
        buf
    }

    /// Le hash PoW de cet en-tête sert aussi d'identifiant de bloc (comme
    /// Bitcoin : block hash = hash de l'en-tête qui satisfait la cible).
    pub fn id(&self, dataset: &Dataset) -> Hash256 {
        ondris_pow::ondris_hash(&self.bytes_for_pow(), self.nonce, dataset)
    }
}
