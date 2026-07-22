//! Cœur de la blockchain Ondris : en-têtes de bloc, transactions, état des
//! comptes, difficulté, et la struct `Chain` qui orchestre tout ça.
//! S'appuie sur `ondris-pow` pour le calcul/la vérification du
//! Proof-of-Work et sur `ondris-primitives` pour les types cryptographiques
//! de base.

pub mod block;
pub mod chain;
pub mod difficulty;
pub mod genesis;
pub mod header;
pub mod rpc_types;
pub mod state;
pub mod transaction;

pub use block::{merkle_root, Block};
pub use chain::Chain;
pub use difficulty::{next_difficulty, target_for_difficulty};
pub use genesis::GenesisConfig;
pub use header::BlockHeader;
pub use rpc_types::{
    AccountInfo, ChainInfo, ErrorResponse, SubmitBlockResponse, SubmitTxResponse, WorkTemplate,
};
pub use state::{Account, ChainState};
pub use transaction::Transaction;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use ondris_primitives::KeyPair;
    use tempfile_shim::TempDir;

    mod tempfile_shim {
        use std::path::{Path, PathBuf};

        /// Mini remplaçant de `tempfile::TempDir` pour ne pas ajouter de
        /// dépendance de test supplémentaire : crée un dossier unique sous
        /// le dossier temp système et le supprime au Drop.
        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                let mut path = std::env::temp_dir();
                let unique = format!("{prefix}-{:?}", std::thread::current().id());
                path.push(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).expect("création du dossier temporaire");
                TempDir(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn test_genesis() -> GenesisConfig {
        let mut g = GenesisConfig::testnet_default();
        g.retarget_window = 4;
        // Difficulté volontairement minuscule pour que le test mine un
        // bloc réel en quelques itérations plutôt qu'en plusieurs secondes.
        g.initial_difficulty = 2;
        g
    }

    #[test]
    fn genesis_initializes_tip_at_zero() {
        let dir = TempDir::new("ondris-core-test-genesis");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let (height, _) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 0);
    }

    #[test]
    fn mine_and_submit_one_block_credits_miner() {
        let dir = TempDir::new("ondris-core-test-mine");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let miner_key = KeyPair::generate();
        let miner_addr = miner_key.address();

        let (mut block, dataset) = chain.work_template(miner_addr, vec![]).unwrap();

        // Mine réellement : incrémente le nonce jusqu'à satisfaire la cible.
        let target = target_for_difficulty(block.header.difficulty);
        loop {
            let hash = block.header.id(&dataset);
            if ondris_pow::meets_target(&hash, &target) {
                break;
            }
            block.header.nonce += 1;
        }

        let hash = chain.submit_block(block).unwrap();
        let (height, tip_hash) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 1);
        assert_eq!(tip_hash, hash);

        let account = chain.state.get_account(&miner_addr).unwrap();
        assert_eq!(account.balance, chain.block_reward(1));
    }

    #[test]
    fn rejects_block_with_wrong_prev_hash() {
        let dir = TempDir::new("ondris-core-test-reject");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let miner_addr = KeyPair::generate().address();
        let (mut block, _dataset) = chain.work_template(miner_addr, vec![]).unwrap();
        block.header.prev_hash = ondris_primitives::Hash256::hash(b"pas le bon prev_hash");
        let result = chain.submit_block(block);
        assert!(result.is_err());
    }
}
