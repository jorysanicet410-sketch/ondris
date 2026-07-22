//! Node de référence Ondris : chaîne + réseau P2P + API RPC HTTP.
//! Testnet uniquement — voir docs/ARCHITECTURE.md pour les limitations
//! connues (pas de gestion de fork, transport P2P non chiffré, mempool
//! minimaliste sans re-file d'attente en cas de bloc non soumis).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use ondris_core::{
    AccountInfo, Block, Chain, ChainInfo, ErrorResponse, GenesisConfig, SubmitBlockResponse,
    SubmitTxResponse, Transaction, WorkTemplate,
};
use ondris_network::{Message, Network, NetworkEvent};
use ondris_primitives::Address;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    name = "ondris-node",
    version,
    about = "Node de référence Ondris (testnet)"
)]
struct Args {
    /// Dossier de données (base sled + config).
    #[arg(long, default_value = "./ondris-data")]
    data_dir: PathBuf,

    /// Adresse d'écoute pour le réseau P2P.
    #[arg(long, default_value = "0.0.0.0:30303")]
    p2p_addr: SocketAddr,

    /// Adresse d'écoute pour l'API RPC HTTP.
    #[arg(long, default_value = "127.0.0.1:8080")]
    rpc_addr: SocketAddr,

    /// Fichier de config genesis JSON (sinon, config testnet par défaut).
    #[arg(long)]
    genesis: Option<PathBuf>,

    /// Pairs (seed nodes) auxquels se connecter au démarrage. Répétable.
    #[arg(long = "peer")]
    peers: Vec<SocketAddr>,
}

struct AppState {
    chain: Chain,
    network: Network,
    mempool: Mutex<Vec<Transaction>>,
}

type SharedState = Arc<AppState>;

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.0.to_string(),
        });
        (StatusCode::BAD_REQUEST, body).into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        AppError(err.into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let genesis = match &args.genesis {
        Some(path) => GenesisConfig::load(path)?,
        None => {
            tracing::warn!(
                "aucun fichier --genesis fourni : utilisation de la config testnet par défaut"
            );
            GenesisConfig::testnet_default()
        }
    };

    std::fs::create_dir_all(&args.data_dir)?;
    let chain = Chain::open(&args.data_dir, genesis.clone())?;
    let (start_height, _) = chain
        .state
        .tip()?
        .expect("le genesis doit être initialisé à l'ouverture");
    tracing::info!(
        "chaîne '{}' ouverte à la hauteur {start_height}",
        genesis.network_name
    );

    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<NetworkEvent>();
    let network = Network::new(genesis.network_name.clone(), events_tx);
    network.set_height(start_height);
    network.listen(args.p2p_addr).await?;

    for peer in &args.peers {
        if let Err(e) = network.connect(*peer).await {
            tracing::warn!("impossible de se connecter au pair {peer}: {e}");
        }
    }

    let state: SharedState = Arc::new(AppState {
        chain,
        network: network.clone(),
        mempool: Mutex::new(Vec::new()),
    });

    let event_state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                NetworkEvent::NewBlock(block) => {
                    let height = block.header.height;
                    match event_state.chain.submit_block(block.clone()) {
                        Ok(hash) => {
                            tracing::info!("bloc {height} accepté depuis le réseau ({hash})");
                            event_state.network.set_height(height);
                            event_state.network.broadcast(Message::NewBlock(block)).await;
                        }
                        Err(e) => tracing::debug!("bloc reçu du réseau rejeté (probablement déjà connu ou hors-ordre): {e}"),
                    }
                }
                NetworkEvent::NewTransaction(tx) => {
                    if tx.is_signature_valid() {
                        event_state.mempool.lock().unwrap().push(tx.clone());
                        event_state
                            .network
                            .broadcast(Message::NewTransaction(tx))
                            .await;
                    }
                }
                NetworkEvent::PeerConnected(addr) => tracing::info!("pair connecté: {addr}"),
                NetworkEvent::PeerDisconnected(addr) => tracing::info!("pair déconnecté: {addr}"),
            }
        }
    });

    let app = Router::new()
        .route("/chain/info", get(chain_info))
        .route("/account/:address", get(get_account))
        .route("/block/height/:height", get(get_block_by_height))
        .route("/work", get(get_work))
        .route("/block/submit", post(submit_block))
        .route("/tx/submit", post(submit_tx))
        .with_state(state);

    tracing::info!("API RPC en écoute sur http://{}", args.rpc_addr);
    let listener = tokio::net::TcpListener::bind(args.rpc_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn chain_info(State(state): State<SharedState>) -> Result<Json<ChainInfo>, AppError> {
    let (height, tip_hash) = state
        .chain
        .state
        .tip()?
        .ok_or_else(|| anyhow::anyhow!("chaîne non initialisée"))?;
    let next_difficulty = state.chain.compute_next_difficulty(height + 1)?;
    let peer_count = state.network.peer_count().await;
    Ok(Json(ChainInfo {
        network: state.chain.genesis.network_name.clone(),
        height,
        tip_hash,
        next_difficulty,
        peer_count,
    }))
}

async fn get_account(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Result<Json<AccountInfo>, AppError> {
    let addr: Address = address.parse()?;
    let account = state.chain.state.get_account(&addr)?;
    Ok(Json(AccountInfo::new(addr, account)))
}

async fn get_block_by_height(
    State(state): State<SharedState>,
    Path(height): Path<u64>,
) -> Result<Json<Block>, AppError> {
    let block = state
        .chain
        .state
        .get_block_by_height(height)?
        .ok_or_else(|| anyhow::anyhow!("bloc {height} introuvable"))?;
    Ok(Json(block))
}

#[derive(serde::Deserialize)]
struct WorkQuery {
    miner: String,
}

async fn get_work(
    State(state): State<SharedState>,
    Query(q): Query<WorkQuery>,
) -> Result<Json<WorkTemplate>, AppError> {
    let miner: Address = q.miner.parse()?;
    let pending: Vec<Transaction> = {
        let mut mempool = state.mempool.lock().unwrap();
        std::mem::take(&mut *mempool)
    };
    let (block, _dataset) = state.chain.work_template(miner, pending)?;
    let next_height = block.header.height;
    let epoch = ondris_pow::epoch_of(next_height);
    let epoch_boundary_hash = if epoch == 0 {
        None
    } else {
        state
            .chain
            .state
            .get_hash_by_height(epoch * ondris_pow::EPOCH_LENGTH)?
    };
    let target = ondris_core::target_for_difficulty(block.header.difficulty);
    Ok(Json(WorkTemplate {
        block,
        target,
        epoch,
        epoch_boundary_hash,
    }))
}

async fn submit_block(
    State(state): State<SharedState>,
    Json(block): Json<Block>,
) -> Result<Json<SubmitBlockResponse>, AppError> {
    let height = block.header.height;
    let hash = state.chain.submit_block(block)?;
    state.network.set_height(height);
    if let Some(stored) = state.chain.state.get_block(&hash)? {
        state.network.broadcast(Message::NewBlock(stored)).await;
    }
    Ok(Json(SubmitBlockResponse {
        block_hash: hash,
        height,
    }))
}

async fn submit_tx(
    State(state): State<SharedState>,
    Json(tx): Json<Transaction>,
) -> Result<Json<SubmitTxResponse>, AppError> {
    if !tx.is_signature_valid() {
        return Err(anyhow::anyhow!("signature invalide").into());
    }
    let hash = tx.hash();
    state.mempool.lock().unwrap().push(tx.clone());
    state.network.broadcast(Message::NewTransaction(tx)).await;
    Ok(Json(SubmitTxResponse { tx_hash: hash }))
}
