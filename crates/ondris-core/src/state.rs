use crate::block::Block;
use ondris_primitives::{Address, Hash256};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub balance: u64,
    /// Prochain `account_nonce` de transaction attendu (protection anti-rejeu).
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Tip {
    height: u64,
    hash: Hash256,
}

/// État de la chaîne persisté sur disque via `sled` : comptes, blocs,
/// index hauteur -> hash, et la tête de chaîne courante.
pub struct ChainState {
    accounts: sled::Tree,
    blocks: sled::Tree,
    heights: sled::Tree,
    meta: sled::Tree,
}

const TIP_KEY: &[u8] = b"tip";

impl ChainState {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let db = sled::open(path)?;
        Ok(ChainState {
            accounts: db.open_tree("accounts")?,
            blocks: db.open_tree("blocks")?,
            heights: db.open_tree("heights")?,
            meta: db.open_tree("meta")?,
        })
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

    pub fn store_block(&self, hash: Hash256, block: &Block) -> anyhow::Result<()> {
        self.blocks.insert(hash.0, serde_json::to_vec(block)?)?;
        self.heights
            .insert(block.header.height.to_be_bytes(), &hash.0)?;
        Ok(())
    }

    pub fn get_block(&self, hash: &Hash256) -> anyhow::Result<Option<Block>> {
        match self.blocks.get(hash.0)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
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
        self.meta.flush()?;
        Ok(())
    }
}
