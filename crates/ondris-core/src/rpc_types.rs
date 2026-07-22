//! DTOs partagés entre le node (serveur RPC), le wallet et le mineur
//! (clients RPC). Vivent dans `ondris-core` pour qu'une seule définition
//! serve tout le monde, plutôt que de dupliquer des structs incompatibles
//! entre binaires.

use crate::block::Block;
use crate::state::Account;
use ondris_primitives::{Address, Hash256};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainInfo {
    pub network: String,
    pub height: u64,
    pub tip_hash: Hash256,
    pub next_difficulty: u64,
    pub peer_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub address: Address,
    pub balance: u64,
    pub nonce: u64,
}

impl AccountInfo {
    pub fn new(address: Address, account: Account) -> Self {
        AccountInfo {
            address,
            balance: account.balance,
            nonce: account.nonce,
        }
    }
}

/// Modèle de travail renvoyé par `GET /work` : un bloc prêt à être miné
/// (nonce = 0, transactions déjà incluses) plus tout ce dont le mineur a
/// besoin pour régénérer localement le dataset de l'époque concernée sans
/// avoir à le télécharger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkTemplate {
    pub block: Block,
    pub target: [u8; 32],
    pub epoch: u64,
    /// Hash du bloc de bordure d'époque, `None` uniquement pour l'époque 0.
    pub epoch_boundary_hash: Option<Hash256>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitBlockResponse {
    pub block_hash: Hash256,
    pub height: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitTxResponse {
    pub tx_hash: Hash256,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
