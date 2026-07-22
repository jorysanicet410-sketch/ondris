use crate::block::Block;
use crate::transaction::Transaction;
use ondris_primitives::{Address, Hash256};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub balance: u64,
    /// Next expected transaction `account_nonce` (anti-replay protection).
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Tip {
    height: u64,
    hash: Hash256,
}

/// Chain state persisted to disk via `sled`: accounts, every block ever
/// seen (whether or not it ended up on the canonical chain), a
/// height -> hash index for the canonical chain only, cumulative chain
/// work per block (for fork-choice), and the current chain tip.
pub struct ChainState {
    accounts: sled::Tree,
    blocks: sled::Tree,
    /// Canonical-chain height -> hash. NOT populated for side-branch
    /// blocks; only updated when a block becomes part of the best chain.
    heights: sled::Tree,
    /// Block hash -> cumulative work (16-byte big-endian u128), for every
    /// stored block regardless of whether it's canonical.
    chainwork: sled::Tree,
    meta: sled::Tree,
    /// Pending transactions not yet included in an accepted block, keyed
    /// by tx hash. Living in `sled` (not an in-memory `Vec`) means a node
    /// restart doesn't lose them.
    mempool: sled::Tree,
}

const TIP_KEY: &[u8] = b"tip";

impl ChainState {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let db = sled::open(path)?;
        Ok(ChainState {
            accounts: db.open_tree("accounts")?,
            blocks: db.open_tree("blocks")?,
            heights: db.open_tree("heights")?,
            chainwork: db.open_tree("chainwork")?,
            meta: db.open_tree("meta")?,
            mempool: db.open_tree("mempool")?,
        })
    }

    /// Adds (or overwrites, if already present) a pending transaction.
    pub fn mempool_insert(&self, tx: &Transaction) -> anyhow::Result<()> {
        self.mempool.insert(tx.hash().0, serde_json::to_vec(tx)?)?;
        Ok(())
    }

    /// Removes a transaction from the pending set — call this once (and
    /// only once) the block containing it is actually accepted onto the
    /// canonical chain. A transaction displaced by a reorg goes back in
    /// via `mempool_insert`, not left removed.
    pub fn mempool_remove(&self, tx_hash: &Hash256) -> anyhow::Result<()> {
        self.mempool.remove(tx_hash.0)?;
        Ok(())
    }

    /// All pending transactions, in no particular order. Read-only —
    /// unlike the old in-memory design, calling this repeatedly (e.g. from
    /// several `GET /work` requests) doesn't consume anything.
    pub fn mempool_all(&self) -> anyhow::Result<Vec<Transaction>> {
        self.mempool
            .iter()
            .values()
            .map(|bytes| Ok(serde_json::from_slice(&bytes?)?))
            .collect()
    }

    pub fn get_account(&self, addr: &Address) -> anyhow::Result<Account> {
        match self.accounts.get(addr.0)? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
            None => Ok(Account::default()),
        }
    }

    pub fn set_account(&self, addr: &Address, account: &Account) -> anyhow::Result<()> {
        self.accounts.insert(addr.0, serde_json::to_vec(account)?)?;
        Ok(())
    }

    pub fn credit(&self, addr: &Address, amount: u64) -> anyhow::Result<()> {
        let mut acc = self.get_account(addr)?;
        acc.balance = acc.balance.saturating_add(amount);
        self.set_account(addr, &acc)
    }

    /// Inverse of `credit`, used to undo a block's effects during a reorg.
    pub fn debit(&self, addr: &Address, amount: u64) -> anyhow::Result<()> {
        let mut acc = self.get_account(addr)?;
        acc.balance = acc.balance.saturating_sub(amount);
        self.set_account(addr, &acc)
    }

    pub fn tip(&self) -> anyhow::Result<Option<(u64, Hash256)>> {
        match self.meta.get(TIP_KEY)? {
            Some(bytes) => {
                let t: Tip = serde_json::from_slice(&bytes)?;
                Ok(Some((t.height, t.hash)))
            }
            None => Ok(None),
        }
    }

    pub fn set_tip(&self, height: u64, hash: Hash256) -> anyhow::Result<()> {
        self.meta
            .insert(TIP_KEY, serde_json::to_vec(&Tip { height, hash })?)?;
        Ok(())
    }

    /// Stores a block's data, keyed by its own hash. Does NOT touch the
    /// canonical height index — a stored block may turn out to live on a
    /// side branch that never becomes the best chain. Call
    /// `set_canonical_height` separately once a block is confirmed part of
    /// the best chain.
    pub fn store_block(&self, hash: Hash256, block: &Block) -> anyhow::Result<()> {
        self.blocks.insert(hash.0, serde_json::to_vec(block)?)?;
        Ok(())
    }

    pub fn has_block(&self, hash: &Hash256) -> anyhow::Result<bool> {
        Ok(self.blocks.contains_key(hash.0)?)
    }

    pub fn get_block(&self, hash: &Hash256) -> anyhow::Result<Option<Block>> {
        match self.blocks.get(hash.0)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    pub fn get_chainwork(&self, hash: &Hash256) -> anyhow::Result<Option<u128>> {
        match self.chainwork.get(hash.0)? {
            Some(bytes) => {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&bytes);
                Ok(Some(u128::from_be_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    pub fn set_chainwork(&self, hash: Hash256, work: u128) -> anyhow::Result<()> {
        self.chainwork.insert(hash.0, &work.to_be_bytes())?;
        Ok(())
    }

    pub fn set_canonical_height(&self, height: u64, hash: Hash256) -> anyhow::Result<()> {
        self.heights.insert(height.to_be_bytes(), &hash.0)?;
        Ok(())
    }

    pub fn remove_canonical_height(&self, height: u64) -> anyhow::Result<()> {
        self.heights.remove(height.to_be_bytes())?;
        Ok(())
    }

    pub fn get_hash_by_height(&self, height: u64) -> anyhow::Result<Option<Hash256>> {
        match self.heights.get(height.to_be_bytes())? {
            Some(bytes) => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(Hash256(arr)))
            }
            None => Ok(None),
        }
    }

    pub fn get_block_by_height(&self, height: u64) -> anyhow::Result<Option<Block>> {
        match self.get_hash_by_height(height)? {
            Some(hash) => self.get_block(&hash),
            None => Ok(None),
        }
    }

    pub fn flush(&self) -> anyhow::Result<()> {
        self.accounts.flush()?;
        self.blocks.flush()?;
        self.heights.flush()?;
        self.chainwork.flush()?;
        self.meta.flush()?;
        self.mempool.flush()?;
        Ok(())
    }
}
