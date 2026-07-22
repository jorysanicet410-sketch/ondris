use crate::block::{merkle_root, Block};
use crate::difficulty::{next_difficulty, target_for_difficulty};
use crate::genesis::GenesisConfig;
use crate::header::BlockHeader;
use crate::state::ChainState;
use ondris_pow::Dataset;
use ondris_primitives::{Address, Hash256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("l'horloge système est avant 1970")
        .as_secs()
}

/// Hash de bloc fixe pour le genesis : il n'est pas issu d'un vrai calcul
/// de PoW (le genesis n'a pas de "bloc précédent" à miner par-dessus).
fn genesis_block_hash(genesis: &GenesisConfig) -> Hash256 {
    Hash256::hash(
        format!(
            "ONDRIS_GENESIS:{}:{}",
            genesis.network_name, genesis.timestamp
        )
        .as_bytes(),
    )
}

pub struct Chain {
    pub state: ChainState,
    pub genesis: GenesisConfig,
    dataset_cache: Mutex<Option<(u64, Arc<Dataset>)>>,
}

impl Chain {
    pub fn open(data_dir: &Path, genesis: GenesisConfig) -> anyhow::Result<Self> {
        let state = ChainState::open(&data_dir.join("db"))?;
        let chain = Chain {
            state,
            genesis,
            dataset_cache: Mutex::new(None),
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
        self.state.store_block(hash, &block)?;
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

    /// Dataset de l'époque contenant `height`, avec mise en cache mémoire
    /// (le dataset ne change qu'une fois tous les `EPOCH_LENGTH` blocs).
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
                        anyhow::anyhow!("bloc de bordure d'époque {boundary_height} introuvable")
                    })?;
            ondris_pow::epoch_seed(Some(boundary_hash))
        };
        let dataset = Arc::new(Dataset::generate(epoch, seed));
        *self.dataset_cache.lock().unwrap() = Some((epoch, dataset.clone()));
        Ok(dataset)
    }

    /// Construit un en-tête prêt à être miné (nonce à 0, à faire varier par
    /// le mineur) pour le prochain bloc, ainsi que le dataset de l'époque
    /// correspondante et la liste de transactions à inclure.
    pub fn work_template(
        &self,
        miner: Address,
        pending_txs: Vec<crate::transaction::Transaction>,
    ) -> anyhow::Result<(Block, Arc<Dataset>)> {
        let (height, prev_hash) = self
            .state
            .tip()?
            .ok_or_else(|| anyhow::anyhow!("chaîne non initialisée"))?;
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

    pub fn compute_next_difficulty(&self, next_height: u64) -> anyhow::Result<u64> {
        let window = self.genesis.retarget_window.max(1);
        if next_height <= window {
            return Ok(self.genesis.initial_difficulty);
        }
        let tip_block = self
            .state
            .get_block_by_height(next_height - 1)?
            .ok_or_else(|| anyhow::anyhow!("bloc {} introuvable", next_height - 1))?;
        let window_start_block = self
            .state
            .get_block_by_height(next_height - 1 - window)?
            .ok_or_else(|| anyhow::anyhow!("bloc de début de fenêtre introuvable"))?;
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

    /// Valide entièrement un bloc (PoW, lien avec le tip, transactions) et
    /// l'applique à l'état si valide. N'accepte que l'extension linéaire du
    /// tip courant (pas de gestion de fork/réorganisation dans cette
    /// première version — documenté comme travail futur).
    pub fn submit_block(&self, block: Block) -> anyhow::Result<Hash256> {
        let (tip_height, tip_hash) = self
            .state
            .tip()?
            .ok_or_else(|| anyhow::anyhow!("chaîne non initialisée"))?;

        anyhow::ensure!(
            block.header.height == tip_height + 1,
            "hauteur de bloc inattendue: reçu {}, attendu {}",
            block.header.height,
            tip_height + 1
        );
        anyhow::ensure!(
            block.header.prev_hash == tip_hash,
            "prev_hash ne correspond pas au tip courant"
        );

        let expected_tx_root = block.compute_tx_root();
        anyhow::ensure!(block.header.tx_root == expected_tx_root, "tx_root invalide");

        let dataset = self.dataset_for_height(block.header.height)?;
        let block_hash = block.header.id(&dataset);
        let target = target_for_difficulty(block.header.difficulty);
        anyhow::ensure!(
            ondris_pow::meets_target(&block_hash, &target),
            "le PoW ne satisfait pas la cible de difficulté"
        );

        let expected_difficulty = self.compute_next_difficulty(block.header.height)?;
        anyhow::ensure!(
            block.header.difficulty == expected_difficulty,
            "difficulté incorrecte pour cette hauteur"
        );

        for tx in &block.transactions {
            anyhow::ensure!(tx.is_signature_valid(), "signature de transaction invalide");
            let sender_addr = tx.from.to_address();
            let account = self.state.get_account(&sender_addr)?;
            anyhow::ensure!(
                tx.account_nonce == account.nonce,
                "nonce de transaction invalide (rejeu ?)"
            );
            anyhow::ensure!(
                account.balance >= tx.amount.saturating_add(tx.fee),
                "solde insuffisant"
            );
        }

        for tx in &block.transactions {
            let sender_addr = tx.from.to_address();
            let mut sender_acc = self.state.get_account(&sender_addr)?;
            sender_acc.balance -= tx.amount + tx.fee;
            sender_acc.nonce += 1;
            self.state.set_account(&sender_addr, &sender_acc)?;
            self.state.credit(&tx.to, tx.amount)?;
        }
        self.state
            .credit(&block.header.miner, self.block_reward(block.header.height))?;

        self.state.store_block(block_hash, &block)?;
        self.state.set_tip(block.header.height, block_hash)?;
        self.state.flush()?;
        Ok(block_hash)
    }
}
