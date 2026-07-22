use crate::block::{merkle_root, Block};
use crate::difficulty::{next_difficulty, target_for_difficulty};
use crate::genesis::GenesisConfig;
use crate::header::BlockHeader;
use crate::state::{Account, ChainState};
use crate::transaction::Transaction;
use ondris_pow::Dataset;
use ondris_primitives::{Address, Hash256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before 1970")
        .as_secs()
}

/// Fixed block hash for genesis: it doesn't come from a real PoW
/// computation (genesis has no "previous block" to mine on top of).
fn genesis_block_hash(genesis: &GenesisConfig) -> Hash256 {
    Hash256::hash(
        format!(
            "ONDRIS_GENESIS:{}:{}",
            genesis.network_name, genesis.timestamp
        )
        .as_bytes(),
    )
}

/// A block's contribution to cumulative chain work. Difficulty already
/// measures "how hard this block was to find" (target = MAX_TARGET /
/// difficulty), so using it directly as work-per-block keeps the
/// fork-choice rule simple and auditable rather than reintroducing a
/// separate unit.
pub fn block_work(difficulty: u64) -> u128 {
    difficulty as u128
}

/// A chain of (hash, block) pairs, ancestor order depending on context —
/// see `fork_paths`.
type BlockChain = Vec<(Hash256, Block)>;

/// Result of submitting a block. Only `Accepted` and `SideBranch` mean the
/// block was valid; `Orphan` means it might still be valid but we can't
/// tell yet because we don't have its parent.
#[derive(Debug)]
pub enum SubmitOutcome {
    /// Now part of the canonical chain (either extended the previous tip
    /// directly, or won a reorg away from it). `requeue` lists
    /// transactions that were on the losing branch and are not on the new
    /// one — the caller (the node) should put them back in its mempool.
    Accepted {
        hash: Hash256,
        height: u64,
        reorged: bool,
        requeue: Vec<Transaction>,
    },
    /// Valid on its own and stored, but not (yet) heavier than the
    /// current tip. Kept around in case a future sibling block extends it
    /// past the tip's cumulative work.
    SideBranch { hash: Hash256, height: u64 },
    /// Already stored; submitting it again is a no-op.
    AlreadyKnown,
    /// We don't have this block's parent yet. The caller should ask peers
    /// for it and resubmit this block once the parent arrives.
    Orphan { missing_parent: Hash256 },
}

pub struct Chain {
    pub state: ChainState,
    pub genesis: GenesisConfig,
    dataset_cache: Mutex<Option<(u64, Arc<Dataset>)>>,
    /// Serializes all state-mutating operations (block acceptance and
    /// reorgs). Without this, two blocks arriving at nearly the same time
    /// from different peers could interleave their reads and writes.
    write_lock: Mutex<()>,
}

impl Chain {
    pub fn open(data_dir: &Path, genesis: GenesisConfig) -> anyhow::Result<Self> {
        let state = ChainState::open(&data_dir.join("db"))?;
        let chain = Chain {
            state,
            genesis,
            dataset_cache: Mutex::new(None),
            write_lock: Mutex::new(()),
        };
        if chain.state.tip()?.is_none() {
            chain.init_genesis()?;
        }
        Ok(chain)
    }

    fn init_genesis(&self) -> anyhow::Result<()> {
        let header = BlockHeader {
            height: 0,
            prev_hash: Hash256::ZERO,
            tx_root: Hash256::ZERO,
            timestamp: self.genesis.timestamp,
            difficulty: self.genesis.initial_difficulty,
            miner: Address([0u8; 20]),
            nonce: 0,
        };
        let block = Block {
            header,
            transactions: vec![],
        };
        let hash = genesis_block_hash(&self.genesis);
        for (addr, amount) in self.genesis.premine_parsed()? {
            self.state.credit(&addr, amount)?;
        }
        self.state
            .set_chainwork(hash, block_work(block.header.difficulty))?;
        self.state.store_block(hash, &block)?;
        self.state.set_canonical_height(0, hash)?;
        self.state.set_tip(0, hash)?;
        self.state.flush()?;
        Ok(())
    }

    pub fn block_reward(&self, height: u64) -> u64 {
        let halvings = height / self.genesis.halving_interval.max(1);
        if halvings >= 64 {
            0
        } else {
            self.genesis.initial_reward >> halvings
        }
    }

    /// Dataset for the epoch containing `height`, cached in memory (the
    /// dataset only changes once every `EPOCH_LENGTH` blocks). Looks up
    /// the epoch boundary block via the CANONICAL height index — correct
    /// as long as competing branches don't span an epoch boundary
    /// (2,048 blocks), which holds for the short races this fork-choice
    /// rule is meant to resolve. A branch that diverges before an epoch
    /// boundary and later wins would need per-branch epoch tracking; not
    /// implemented here, documented in docs/ARCHITECTURE.md.
    pub fn dataset_for_height(&self, height: u64) -> anyhow::Result<Arc<Dataset>> {
        let epoch = ondris_pow::epoch_of(height);
        {
            let cache = self.dataset_cache.lock().unwrap();
            if let Some((cached_epoch, ds)) = cache.as_ref() {
                if *cached_epoch == epoch {
                    return Ok(ds.clone());
                }
            }
        }
        let seed = if epoch == 0 {
            ondris_pow::epoch_seed(None)
        } else {
            let boundary_height = epoch * ondris_pow::EPOCH_LENGTH;
            let boundary_hash =
                self.state
                    .get_hash_by_height(boundary_height)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("epoch boundary block {boundary_height} not found")
                    })?;
            ondris_pow::epoch_seed(Some(boundary_hash))
        };
        let dataset = Arc::new(Dataset::generate(epoch, seed));
        *self.dataset_cache.lock().unwrap() = Some((epoch, dataset.clone()));
        Ok(dataset)
    }

    /// Builds a header ready to be mined (nonce at 0, to be varied by the
    /// miner) for the next block, along with the matching epoch's dataset
    /// and the list of transactions to include. Always builds on the
    /// current canonical tip.
    pub fn work_template(
        &self,
        miner: Address,
        pending_txs: Vec<Transaction>,
    ) -> anyhow::Result<(Block, Arc<Dataset>)> {
        let (height, prev_hash) = self
            .state
            .tip()?
            .ok_or_else(|| anyhow::anyhow!("chain not initialized"))?;
        let next_height = height + 1;
        let dataset = self.dataset_for_height(next_height)?;
        let difficulty = self.compute_next_difficulty(next_height)?;

        let tx_root = merkle_root(&pending_txs.iter().map(|t| t.hash()).collect::<Vec<_>>());
        let header = BlockHeader {
            height: next_height,
            prev_hash,
            tx_root,
            timestamp: now_secs(),
            difficulty,
            miner,
            nonce: 0,
        };
        Ok((
            Block {
                header,
                transactions: pending_txs,
            },
            dataset,
        ))
    }

    /// Difficulty for the block after the CANONICAL tip. Used by
    /// `work_template` and the `/chain/info` RPC, which only ever care
    /// about the chain we're actually building on.
    pub fn compute_next_difficulty(&self, next_height: u64) -> anyhow::Result<u64> {
        let window = self.genesis.retarget_window.max(1);
        if next_height <= window {
            return Ok(self.genesis.initial_difficulty);
        }
        let tip_block = self
            .state
            .get_block_by_height(next_height - 1)?
            .ok_or_else(|| anyhow::anyhow!("block {} not found", next_height - 1))?;
        let window_start_block = self
            .state
            .get_block_by_height(next_height - 1 - window)?
            .ok_or_else(|| anyhow::anyhow!("window start block not found"))?;
        let actual_timespan = tip_block
            .header
            .timestamp
            .saturating_sub(window_start_block.header.timestamp);
        Ok(next_difficulty(
            tip_block.header.difficulty,
            actual_timespan,
            self.genesis.target_block_time_secs,
            window,
        ))
    }

    /// Walks `depth` blocks back from `from` along its own prev_hash
    /// chain (NOT the canonical height index), so it works for blocks on
    /// a side branch too.
    fn block_ancestor(&self, from: &Block, depth: u64) -> anyhow::Result<Option<Block>> {
        let mut current = from.clone();
        for _ in 0..depth {
            match self.state.get_block(&current.header.prev_hash)? {
                Some(b) => current = b,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// Difficulty expected for the block right after `parent`, computed
    /// from `parent`'s own branch — works whether `parent` is the
    /// canonical tip or a side-branch block, unlike `compute_next_difficulty`.
    fn expected_difficulty_after(&self, parent: &Block) -> anyhow::Result<u64> {
        let next_height = parent.header.height + 1;
        let window = self.genesis.retarget_window.max(1);
        if next_height <= window {
            return Ok(self.genesis.initial_difficulty);
        }
        let window_start = self.block_ancestor(parent, window)?.ok_or_else(|| {
            anyhow::anyhow!("not enough ancestors on this branch to retarget difficulty")
        })?;
        let actual_timespan = parent
            .header
            .timestamp
            .saturating_sub(window_start.header.timestamp);
        Ok(next_difficulty(
            parent.header.difficulty,
            actual_timespan,
            self.genesis.target_block_time_secs,
            window,
        ))
    }

    /// Validates a block on its own merits and, if it turns out to
    /// out-work the current tip, reorganizes the canonical chain onto it.
    /// Accepts blocks that don't directly extend the tip (side branches),
    /// as long as their parent is already known — this is what lets the
    /// network survive two miners finding a block at the same time.
    pub fn submit_block(&self, block: Block) -> anyhow::Result<SubmitOutcome> {
        let _guard = self.write_lock.lock().unwrap();

        anyhow::ensure!(
            block.header.height > 0,
            "refusing to accept a second genesis block"
        );

        let dataset = self.dataset_for_height(block.header.height)?;
        let hash = block.header.id(&dataset);

        if self.state.has_block(&hash)? {
            return Ok(SubmitOutcome::AlreadyKnown);
        }

        let expected_tx_root = block.compute_tx_root();
        anyhow::ensure!(block.header.tx_root == expected_tx_root, "invalid tx_root");

        let target = target_for_difficulty(block.header.difficulty);
        anyhow::ensure!(
            ondris_pow::meets_target(&hash, &target),
            "PoW does not meet the difficulty target"
        );

        for tx in &block.transactions {
            anyhow::ensure!(tx.is_signature_valid(), "invalid transaction signature");
        }

        let parent = match self.state.get_block(&block.header.prev_hash)? {
            Some(p) => p,
            None => {
                return Ok(SubmitOutcome::Orphan {
                    missing_parent: block.header.prev_hash,
                })
            }
        };
        anyhow::ensure!(
            parent.header.height + 1 == block.header.height,
            "block height {} does not follow its stated parent (height {})",
            block.header.height,
            parent.header.height
        );

        let expected_difficulty = self.expected_difficulty_after(&parent)?;
        anyhow::ensure!(
            block.header.difficulty == expected_difficulty,
            "incorrect difficulty for this branch/height: got {}, expected {}",
            block.header.difficulty,
            expected_difficulty
        );

        let parent_work = self
            .state
            .get_chainwork(&block.header.prev_hash)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing chainwork for known parent {}",
                    block.header.prev_hash
                )
            })?;
        let this_work = parent_work + block_work(block.header.difficulty);

        self.state.store_block(hash, &block)?;
        self.state.set_chainwork(hash, this_work)?;
        self.state.flush()?;

        let (_, tip_hash) = self
            .state
            .tip()?
            .ok_or_else(|| anyhow::anyhow!("chain not initialized"))?;
        let tip_work = self
            .state
            .get_chainwork(&tip_hash)?
            .ok_or_else(|| anyhow::anyhow!("missing chainwork for current tip"))?;

        if this_work > tip_work {
            let reorged = tip_hash != block.header.prev_hash;
            let requeue = self.reconsider_tip(tip_hash, hash, block.header.height)?;
            Ok(SubmitOutcome::Accepted {
                hash,
                height: block.header.height,
                reorged,
                requeue,
            })
        } else {
            Ok(SubmitOutcome::SideBranch {
                hash,
                height: block.header.height,
            })
        }
    }

    /// Splits two chain tips into the blocks that must be undone (from the
    /// old tip down to, but not including, the common ancestor — in
    /// tip-first order) and the blocks that must be applied (from just
    /// after the common ancestor up to the new tip — in ancestor-first
    /// order).
    fn fork_paths(
        &self,
        old_tip: Hash256,
        new_tip: Hash256,
    ) -> anyhow::Result<(BlockChain, BlockChain)> {
        let mut undo = Vec::new();
        let mut apply = Vec::new();

        let mut old_hash = old_tip;
        let mut new_hash = new_tip;
        let mut old_block = self
            .state
            .get_block(&old_hash)?
            .ok_or_else(|| anyhow::anyhow!("missing block {old_hash}"))?;
        let mut new_block = self
            .state
            .get_block(&new_hash)?
            .ok_or_else(|| anyhow::anyhow!("missing block {new_hash}"))?;

        while old_block.header.height > new_block.header.height {
            undo.push((old_hash, old_block.clone()));
            old_hash = old_block.header.prev_hash;
            old_block = self
                .state
                .get_block(&old_hash)?
                .ok_or_else(|| anyhow::anyhow!("missing ancestor {old_hash}"))?;
        }
        while new_block.header.height > old_block.header.height {
            apply.push((new_hash, new_block.clone()));
            new_hash = new_block.header.prev_hash;
            new_block = self
                .state
                .get_block(&new_hash)?
                .ok_or_else(|| anyhow::anyhow!("missing ancestor {new_hash}"))?;
        }
        while old_hash != new_hash {
            undo.push((old_hash, old_block.clone()));
            old_hash = old_block.header.prev_hash;
            old_block = self
                .state
                .get_block(&old_hash)?
                .ok_or_else(|| anyhow::anyhow!("missing ancestor {old_hash}"))?;

            apply.push((new_hash, new_block.clone()));
            new_hash = new_block.header.prev_hash;
            new_block = self
                .state
                .get_block(&new_hash)?
                .ok_or_else(|| anyhow::anyhow!("missing ancestor {new_hash}"))?;
        }

        apply.reverse();
        Ok((undo, apply))
    }

    /// Re-points the canonical chain from `old_tip` to `new_tip`. Simulates
    /// the whole undo+apply sequence in memory first and only writes to
    /// `self.state` if every transaction on the new branch actually
    /// checks out — a bad new branch can never leave the database
    /// half-updated.
    fn reconsider_tip(
        &self,
        old_tip: Hash256,
        new_tip: Hash256,
        new_tip_height: u64,
    ) -> anyhow::Result<Vec<Transaction>> {
        let (undo_list, apply_list) = self.fork_paths(old_tip, new_tip)?;

        let mut overlay: HashMap<Address, Account> = HashMap::new();
        macro_rules! get_acc {
            ($addr:expr) => {{
                match overlay.get(&$addr) {
                    Some(a) => *a,
                    None => self.state.get_account(&$addr)?,
                }
            }};
        }

        for (_, block) in &undo_list {
            let mut miner_acc = get_acc!(block.header.miner);
            miner_acc.balance = miner_acc
                .balance
                .saturating_sub(self.block_reward(block.header.height));
            overlay.insert(block.header.miner, miner_acc);

            for tx in block.transactions.iter().rev() {
                let mut to_acc = get_acc!(tx.to);
                to_acc.balance = to_acc.balance.saturating_sub(tx.amount);
                overlay.insert(tx.to, to_acc);

                let sender = tx.from.to_address();
                let mut sender_acc = get_acc!(sender);
                sender_acc.balance = sender_acc.balance.saturating_add(tx.amount + tx.fee);
                sender_acc.nonce = sender_acc.nonce.saturating_sub(1);
                overlay.insert(sender, sender_acc);
            }
        }

        for (hash, block) in &apply_list {
            for tx in &block.transactions {
                let sender = tx.from.to_address();
                let mut sender_acc = get_acc!(sender);
                anyhow::ensure!(
                    tx.account_nonce == sender_acc.nonce,
                    "reorg to {hash}: invalid tx nonce in block {} (rolled back the wrong way?)",
                    block.header.height
                );
                anyhow::ensure!(
                    sender_acc.balance >= tx.amount.saturating_add(tx.fee),
                    "reorg to {hash}: insufficient balance for a transaction in block {}",
                    block.header.height
                );
                sender_acc.balance -= tx.amount + tx.fee;
                sender_acc.nonce += 1;
                overlay.insert(sender, sender_acc);

                let mut to_acc = get_acc!(tx.to);
                to_acc.balance = to_acc.balance.saturating_add(tx.amount);
                overlay.insert(tx.to, to_acc);
            }
            let mut miner_acc = get_acc!(block.header.miner);
            miner_acc.balance = miner_acc
                .balance
                .saturating_add(self.block_reward(block.header.height));
            overlay.insert(block.header.miner, miner_acc);
        }

        // Simulation succeeded end to end: commit for real.
        for (addr, acc) in &overlay {
            self.state.set_account(addr, acc)?;
        }
        for (_, block) in &undo_list {
            self.state.remove_canonical_height(block.header.height)?;
        }
        for (hash, block) in &apply_list {
            self.state
                .set_canonical_height(block.header.height, *hash)?;
        }
        self.state.set_tip(new_tip_height, new_tip)?;
        self.state.flush()?;

        let applied_hashes: HashSet<Hash256> = apply_list
            .iter()
            .flat_map(|(_, b)| b.transactions.iter().map(|t| t.hash()))
            .collect();
        let mut requeue = Vec::new();
        for (_, block) in &undo_list {
            for tx in &block.transactions {
                if !applied_hashes.contains(&tx.hash()) {
                    requeue.push(tx.clone());
                }
            }
        }
        Ok(requeue)
    }
}
