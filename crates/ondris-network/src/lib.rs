//! Minimal P2P network for Ondris: block/transaction gossip over TCP with
//! length-prefixed JSON messages, plus a `GetBlock`/`BlockResponse` pair
//! the node uses to fetch a missing parent when a block arrives out of
//! order. No automatic peer discovery (DHT) in this first version: the
//! peer list (seed nodes) is supplied via config at node startup.
//! Documented as future work: peer discovery, transport encryption
//! (currently plaintext, fine for a testnet but not for a mainnet with
//! real value at stake).

use ondris_core::{Block, Transaction};
use ondris_primitives::Hash256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    Handshake {
        version: u32,
        network: String,
        height: u64,
    },
    NewBlock(Block),
    NewTransaction(Transaction),
    /// Asks a peer for a block by hash — used to resolve orphans (a block
    /// received before its parent).
    GetBlock(Hash256),
    BlockResponse(Option<Block>),
    Ping,
    Pong,
}

#[derive(Debug)]
pub enum NetworkEvent {
    PeerConnected(SocketAddr),
    PeerDisconnected(SocketAddr),
    NewBlock(Block),
    NewTransaction(Transaction),
    /// A peer asked us for a block; `SocketAddr` is who to answer.
    GetBlockRequest(SocketAddr, Hash256),
    BlockResponse(Option<Block>),
}

type PeerMap = Arc<Mutex<HashMap<SocketAddr, mpsc::UnboundedSender<Message>>>>;

#[derive(Clone)]
pub struct Network {
    peers: PeerMap,
    events_tx: mpsc::UnboundedSender<NetworkEvent>,
    network_name: String,
    my_height: Arc<AtomicU64>,
}

impl Network {
    pub fn new(network_name: String, events_tx: mpsc::UnboundedSender<NetworkEvent>) -> Self {
        Network {
            peers: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
            network_name,
            my_height: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_height(&self, height: u64) {
        self.my_height.store(height, Ordering::Relaxed);
    }

    /// Starts listening for inbound connections on `addr` (background
    /// task, does not block).
    pub async fn listen(&self, addr: SocketAddr) -> anyhow::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!("P2P network listening on {addr}");
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        let this2 = this.clone();
                        tokio::spawn(async move {
                            if let Err(e) = this2.handle_connection(stream, peer_addr).await {
                                tracing::warn!("inbound connection {peer_addr} closed: {e}");
                            }
                        });
                    }
                    Err(e) => tracing::warn!("accept() error: {e}"),
                }
            }
        });
        Ok(())
    }

    /// Opens an outbound connection to a peer (config seed node).
    pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<()> {
        let stream = TcpStream::connect(addr).await?;
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.handle_connection(stream, addr).await {
                tracing::warn!("outbound connection to {addr} closed: {e}");
            }
        });
        Ok(())
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
        peer_addr: SocketAddr,
    ) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        self.peers.lock().await.insert(peer_addr, tx);
        let _ = self.events_tx.send(NetworkEvent::PeerConnected(peer_addr));

        let (mut reader, mut writer) = stream.into_split();

        let handshake = Message::Handshake {
            version: PROTOCOL_VERSION,
            network: self.network_name.clone(),
            height: self.my_height.load(Ordering::Relaxed),
        };
        write_message(&mut writer, &handshake).await?;

        let write_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if write_message(&mut writer, &msg).await.is_err() {
                    break;
                }
            }
        });

        let result: anyhow::Result<()> = async {
            loop {
                let msg = read_message(&mut reader).await?;
                match msg {
                    Message::Handshake { network, .. } => {
                        anyhow::ensure!(
                            network == self.network_name,
                            "different network: {network}"
                        );
                    }
                    Message::NewBlock(block) => {
                        let _ = self.events_tx.send(NetworkEvent::NewBlock(block));
                    }
                    Message::NewTransaction(tx) => {
                        let _ = self.events_tx.send(NetworkEvent::NewTransaction(tx));
                    }
                    Message::GetBlock(hash) => {
                        let _ = self
                            .events_tx
                            .send(NetworkEvent::GetBlockRequest(peer_addr, hash));
                    }
                    Message::BlockResponse(block) => {
                        let _ = self.events_tx.send(NetworkEvent::BlockResponse(block));
                    }
                    Message::Ping => self.send_to(peer_addr, Message::Pong).await,
                    Message::Pong => {}
                }
            }
        }
        .await;

        self.peers.lock().await.remove(&peer_addr);
        let _ = self
            .events_tx
            .send(NetworkEvent::PeerDisconnected(peer_addr));
        write_task.abort();
        result
    }

    pub async fn send_to(&self, addr: SocketAddr, msg: Message) {
        let peers = self.peers.lock().await;
        if let Some(tx) = peers.get(&addr) {
            let _ = tx.send(msg);
        }
    }

    pub async fn broadcast(&self, msg: Message) {
        let peers = self.peers.lock().await;
        for tx in peers.values() {
            let _ = tx.send(msg.clone());
        }
    }

    pub async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }
}

async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, msg: &Message) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    let len = bytes.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Message> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(
        len <= 64 * 1024 * 1024,
        "received message too large ({len} bytes)"
    );
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let msg: Message = serde_json::from_slice(&buf)?;
    Ok(msg)
}
