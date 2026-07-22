//! Core of the Ondris blockchain: block headers, transactions, account
//! state, difficulty, and the `Chain` struct that orchestrates all of it.
//! Relies on `ondris-pow` for computing/verifying Proof-of-Work and on
//! `ondris-primitives` for base cryptographic types.

pub mod block;
pub mod chain;
pub mod difficulty;
pub mod genesis;
pub mod header;
pub mod rpc_types;
pub mod state;
pub mod transaction;

pub use block::{merkle_root, Block};
pub use chain::{block_work, Chain, SubmitOutcome};
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
    use ondris_primitives::{Address, Hash256, KeyPair};
    use tempfile_shim::TempDir;

    mod tempfile_shim {
        use std::path::{Path, PathBuf};

        /// Tiny stand-in for `tempfile::TempDir` so we don't need an extra
        /// test dependency: creates a unique directory under the system
        /// temp dir and removes it on Drop.
        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                let mut path = std::env::temp_dir();
                let unique = format!("{prefix}-{:?}", std::thread::current().id());
                path.push(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).expect("failed to create temp directory");
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
        // Deliberately tiny difficulty so the test mines a real block in
        // a handful of iterations rather than several seconds.
        g.initial_difficulty = 2;
        g
    }

    #[test]
    fn mempool_survives_a_node_restart() {
        let dir = TempDir::new("ondris-core-test-mempool-persist");
        let sender = KeyPair::generate();
        let mut tx = Transaction::new_unsigned(sender.public(), sender.address(), 1, 0, 0);
        tx.sign(&sender);
        let tx_hash = tx.hash();

        {
            let chain = Chain::open(dir.path(), test_genesis()).unwrap();
            chain.state.mempool_insert(&tx).unwrap();
            chain.state.flush().unwrap();
            // `chain` drops here, simulating the node process exiting.
        }

        let reopened = Chain::open(dir.path(), test_genesis()).unwrap();
        let pending = reopened.state.mempool_all().unwrap();
        assert_eq!(pending.len(), 1, "the transaction must survive a restart");
        assert_eq!(pending[0].hash(), tx_hash);

        reopened.state.mempool_remove(&tx_hash).unwrap();
        assert!(reopened.state.mempool_all().unwrap().is_empty());
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

        // Actually mine: increment the nonce until it satisfies the target.
        let target = target_for_difficulty(block.header.difficulty);
        loop {
            let hash = block.header.id(&dataset);
            if ondris_pow::meets_target(&hash, &target) {
                break;
            }
            block.header.nonce += 1;
        }

        let outcome = chain.submit_block(block).unwrap();
        let hash = match outcome {
            SubmitOutcome::Accepted {
                hash,
                height,
                reorged,
                ..
            } => {
                assert_eq!(height, 1);
                assert!(!reorged, "extending an empty chain isn't a reorg");
                hash
            }
            other => panic!("expected Accepted, got {other:?}"),
        };
        let (height, tip_hash) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 1);
        assert_eq!(tip_hash, hash);

        let account = chain.state.get_account(&miner_addr).unwrap();
        assert_eq!(account.balance, chain.block_reward(1));
    }

    /// Mines a valid child of `prev_hash` at `height` with a fixed
    /// difficulty, for constructing competing branches in tests.
    fn mine_child(
        chain: &Chain,
        prev_hash: Hash256,
        height: u64,
        difficulty: u64,
        miner: Address,
    ) -> Block {
        let dataset = chain.dataset_for_height(height).unwrap();
        let header = BlockHeader {
            height,
            prev_hash,
            tx_root: merkle_root(&[]),
            timestamp: chain::now_secs(),
            difficulty,
            miner,
            nonce: 0,
        };
        let mut block = Block {
            header,
            transactions: vec![],
        };
        let target = target_for_difficulty(difficulty);
        loop {
            let hash = block.header.id(&dataset);
            if ondris_pow::meets_target(&hash, &target) {
                return block;
            }
            block.header.nonce += 1;
        }
    }

    #[test]
    fn unknown_parent_is_reported_as_orphan_not_an_error() {
        let dir = TempDir::new("ondris-core-test-orphan");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let miner_addr = KeyPair::generate().address();
        let difficulty = chain.compute_next_difficulty(1).unwrap();
        let missing = Hash256::hash(b"a parent we never stored");
        // Mine a genuinely valid block ON TOP of the missing parent —
        // prev_hash is part of what gets hashed, so it has to be present
        // from the start rather than swapped in after mining.
        let block = mine_child(&chain, missing, 1, difficulty, miner_addr);

        let outcome = chain.submit_block(block).unwrap();
        match outcome {
            SubmitOutcome::Orphan { missing_parent } => assert_eq!(missing_parent, missing),
            other => panic!("expected Orphan, got {other:?}"),
        }
        // An orphan must never move the tip.
        let (height, _) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 0);
    }

    #[test]
    fn equal_work_side_branch_does_not_move_the_tip() {
        let dir = TempDir::new("ondris-core-test-sidebranch");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let (_, genesis_hash) = chain.state.tip().unwrap().unwrap();
        let miner_a = KeyPair::generate().address();
        let miner_b = KeyPair::generate().address();
        let difficulty = chain.compute_next_difficulty(1).unwrap();

        let a1 = mine_child(&chain, genesis_hash, 1, difficulty, miner_a);
        let a1_outcome = chain.submit_block(a1).unwrap();
        let a1_hash = match a1_outcome {
            SubmitOutcome::Accepted { hash, .. } => hash,
            other => panic!("expected Accepted, got {other:?}"),
        };

        let b1 = mine_child(&chain, genesis_hash, 1, difficulty, miner_b);
        let b1_outcome = chain.submit_block(b1).unwrap();
        assert!(
            matches!(b1_outcome, SubmitOutcome::SideBranch { .. }),
            "equal work must not win: {b1_outcome:?}"
        );

        let (height, tip_hash) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 1);
        assert_eq!(
            tip_hash, a1_hash,
            "first-seen branch should still be the tip on a tie"
        );
    }

    #[test]
    fn heavier_branch_triggers_a_reorg_and_rolls_back_the_loser() {
        let dir = TempDir::new("ondris-core-test-reorg");
        let chain = Chain::open(dir.path(), test_genesis()).unwrap();
        let (_, genesis_hash) = chain.state.tip().unwrap().unwrap();
        let miner_a = KeyPair::generate().address();
        let miner_b = KeyPair::generate().address();
        let d1 = chain.compute_next_difficulty(1).unwrap();

        // Branch A: genesis -> A1 (one block of work).
        let a1 = mine_child(&chain, genesis_hash, 1, d1, miner_a);
        match chain.submit_block(a1).unwrap() {
            SubmitOutcome::Accepted {
                height, reorged, ..
            } => {
                assert_eq!(height, 1);
                assert!(!reorged);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
        assert_eq!(
            chain.state.get_account(&miner_a).unwrap().balance,
            chain.block_reward(1)
        );

        // Branch B: genesis -> B1 -> B2 (two blocks of work — heavier than A).
        let b1 = mine_child(&chain, genesis_hash, 1, d1, miner_b);
        let b1_hash = match chain.submit_block(b1.clone()).unwrap() {
            SubmitOutcome::SideBranch { hash, .. } => hash,
            other => panic!("expected SideBranch, got {other:?}"),
        };
        // Tip hasn't moved yet: still branch A's block, miner_a still credited.
        assert_eq!(
            chain.state.get_account(&miner_a).unwrap().balance,
            chain.block_reward(1)
        );

        let b2 = mine_child(&chain, b1_hash, 2, d1, miner_b);
        match chain.submit_block(b2).unwrap() {
            SubmitOutcome::Accepted {
                height,
                reorged,
                requeue,
                ..
            } => {
                assert_eq!(height, 2);
                assert!(reorged, "B2 should have reorganized the chain away from A1");
                assert!(
                    requeue.is_empty(),
                    "neither branch had any transactions here"
                );
            }
            other => panic!("expected Accepted, got {other:?}"),
        }

        let (height, _) = chain.state.tip().unwrap().unwrap();
        assert_eq!(height, 2);
        // A1's reward must be rolled back...
        assert_eq!(chain.state.get_account(&miner_a).unwrap().balance, 0);
        // ...and miner_b must be credited for both B1 and B2.
        assert_eq!(
            chain.state.get_account(&miner_b).unwrap().balance,
            chain.block_reward(1) + chain.block_reward(2)
        );
    }
}
