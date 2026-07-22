//! Ondris reference node: chain + P2P network + HTTP RPC API.
//! Testnet only — see docs/ARCHITECTURE.md for known limitations
//! (unencrypted P2P transport, static peer list only).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use ondris_core::{
    AccountInfo, Block, Chain, ChainInfo, ErrorResponse, GenesisConfig, SubmitBlockResponse,
    SubmitOutcome, SubmitTxResponse, Transaction, WorkTemplate,
};
use ondris_network::{Message, Network, NetworkEvent};
use ondris_primitives::{Address, Hash256};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};

#[derive(Parser, Debug)]
#[command(
    name = "ondris-node",
    version,
    about = "Ondris reference node (testnet)"
)]
struct Args {
    /// Data directory (sled database + config).
    #[arg(long, default_value = "./ondris-data")]
    data_dir: PathBuf,

    /// Listen address for the P2P network.
    #[arg(long, default_value = "0.0.0.0:30303")]
    p2p_addr: SocketAddr,

    /// Listen address for the HTTP RPC API.
    #[arg(long, default_value = "127.0.0.1:8080")]
    rpc_addr: SocketAddr,

    /// JSON genesis config file (otherwise, the default testnet config is used).
    #[arg(long)]
    genesis: Option<PathBuf>,

    /// Peers (seed nodes) to connect to at startup. Repeatable.
    #[arg(long = "peer")]
    peers: Vec<SocketAddr>,
}

struct AppState {
    chain: Chain,
    network: Network,
    mempool: Mutex<Vec<Transaction>>,
    /// Blocks received before their parent, keyed by the parent hash
    /// they're waiting on. Retried once that parent is accepted.
    orphans: Mutex<HashMap<Hash256, Vec<Block>>>,
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
            tracing::warn!("no --genesis file provided: using the default testnet config");
            GenesisConfig::testnet_default()
        }
    };

    std::fs::create_dir_all(&args.data_dir)?;
    let chain = Chain::open(&args.data_dir, genesis.clone())?;
    let (start_height, _) = chain
        .state
        .tip()?
        .expect("genesis must be initialized on open");
    tracing::info!(
        "chain '{}' opened at height {start_height}",
        genesis.network_name
    );

    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<NetworkEvent>();
    let network = Network::new(genesis.network_name.clone(), events_tx);
    network.set_height(start_height);
    network.listen(args.p2p_addr).await?;

    for peer in &args.peers {
        if let Err(e) = network.connect(*peer).await {
            tracing::warn!("could not connect to peer {peer}: {e}");
        }
    }

    let state: SharedState = Arc::new(AppState {
        chain,
        network: network.clone(),
        mempool: Mutex::new(Vec::new()),
        orphans: Mutex::new(HashMap::new()),
    });

    let event_state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                NetworkEvent::NewBlock(block) => {
                    let _ = accept_and_broadcast(&event_state, block).await;
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
                NetworkEvent::GetBlockRequest(peer_addr, hash) => {
                    let block = event_state.chain.state.get_block(&hash).ok().flatten();
                    event_state
                        .network
                        .send_to(peer_addr, Message::BlockResponse(block))
                        .await;
                }
                NetworkEvent::BlockResponse(Some(block)) => {
                    let _ = accept_and_broadcast(&event_state, block).await;
                }
                NetworkEvent::BlockResponse(None) => {}
                NetworkEvent::PeerConnected(addr) => tracing::info!("peer connected: {addr}"),
                NetworkEvent::PeerDisconnected(addr) => tracing::info!("peer disconnected: {addr}"),
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
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    tracing::info!("RPC API listening on http://{}", args.rpc_addr);
    let listener = tokio::net::TcpListener::bind(args.rpc_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Feeds one block through consensus and reacts to the outcome: broadcasts
/// accepted/side-branch blocks, re-queues transactions that fell off a
/// losing branch, and — if the block turns out to be an orphan — buffers
/// it and asks peers for the missing parent. If accepting it unlocks any
/// previously-buffered orphans (or a whole cascade of them), those are
/// processed too. Returns the outcome for the block that was passed in
/// specifically, not for any cascaded orphans.
async fn accept_and_broadcast(state: &SharedState, block: Block) -> anyhow::Result<SubmitOutcome> {
    let outcome = state.chain.submit_block(block.clone())?;
    handle_outcome(state, &block, &outcome).await;

    if let SubmitOutcome::Accepted { hash, .. } = &outcome {
        let mut queue: VecDeque<Hash256> = VecDeque::new();
        queue.push_back(*hash);
        while let Some(h) = queue.pop_front() {
            let waiting = state.orphans.lock().unwrap().remove(&h);
            let Some(children) = waiting else { continue };
            for child in children {
                match state.chain.submit_block(child.clone()) {
                    Ok(child_outcome) => {
                        handle_outcome(state, &child, &child_outcome).await;
                        if let SubmitOutcome::Accepted { hash: h2, .. } = child_outcome {
                            queue.push_back(h2);
                        }
                    }
                    Err(e) => tracing::debug!("rejected a buffered orphan block: {e}"),
                }
            }
        }
    }

    Ok(outcome)
}

async fn handle_outcome(state: &SharedState, block: &Block, outcome: &SubmitOutcome) {
    match outcome {
        SubmitOutcome::Accepted {
            hash,
            height,
            reorged,
            requeue,
        } => {
            tracing::info!(
                "block {height} accepted{} ({hash})",
                if *reorged { " via reorg" } else { "" }
            );
            state.network.set_height(*height);
            state
                .network
                .broadcast(Message::NewBlock(block.clone()))
                .await;
            if !requeue.is_empty() {
                let mut mempool = state.mempool.lock().unwrap();
                for tx in requeue {
                    if !mempool.iter().any(|t| t.hash() == tx.hash()) {
                        mempool.push(tx.clone());
                    }
                }
            }
        }
        SubmitOutcome::SideBranch { height, .. } => {
            tracing::debug!("block {height} stored as a side branch, tip unchanged");
            state
                .network
                .broadcast(Message::NewBlock(block.clone()))
                .await;
        }
        SubmitOutcome::AlreadyKnown => {}
        SubmitOutcome::Orphan { missing_parent } => {
            tracing::debug!("buffering orphan block, requesting parent {missing_parent}");
            state
                .orphans
                .lock()
                .unwrap()
                .entry(*missing_parent)
                .or_default()
                .push(block.clone());
            state
                .network
                .broadcast(Message::GetBlock(*missing_parent))
                .await;
        }
    }
}

async fn chain_info(State(state): State<SharedState>) -> Result<Json<ChainInfo>, AppError> {
    let (height, tip_hash) = state
        .chain
        .state
        .tip()?
        .ok_or_else(|| anyhow::anyhow!("chain not initialized"))?;
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
        .ok_or_else(|| anyhow::anyhow!("block {height} not found"))?;
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
    let outcome = accept_and_broadcast(&state, block).await?;
    match outcome {
        SubmitOutcome::Accepted { hash, height, .. }
        | SubmitOutcome::SideBranch { hash, height } => Ok(Json(SubmitBlockResponse {
            block_hash: hash,
            height,
        })),
        SubmitOutcome::AlreadyKnown => Err(anyhow::anyhow!("block already known").into()),
        SubmitOutcome::Orphan { missing_parent } => Err(anyhow::anyhow!(
            "unknown parent block {missing_parent}; the node will try to fetch it from peers"
        )
        .into()),
    }
}

async fn submit_tx(
    State(state): State<SharedState>,
    Json(tx): Json<Transaction>,
) -> Result<Json<SubmitTxResponse>, AppError> {
    if !tx.is_signature_valid() {
        return Err(anyhow::anyhow!("invalid signature").into());
    }
    let hash = tx.hash();
    state.mempool.lock().unwrap().push(tx.clone());
    state.network.broadcast(Message::NewTransaction(tx)).await;
    Ok(Json(SubmitTxResponse { tx_hash: hash }))
}
