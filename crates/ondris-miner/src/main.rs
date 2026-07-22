//! Mineur CPU de référence pour Ondris. Sert à valider les règles de
//! consensus et à tester le réseau ; ce n'est PAS un mineur GPU optimisé.
//! Un kernel OpenCL/CUDA reprenant la même logique (dataset partagé,
//! accès mémoire parallèles) est le travail suivant documenté dans
//! docs/ALGORITHM.md.

use clap::Parser;
use ondris_core::{Block, WorkTemplate};
use ondris_pow::Dataset;
use ondris_primitives::Address;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "ondris-miner",
    version,
    about = "Mineur CPU de référence pour Ondris (testnet)"
)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    node: String,

    /// Adresse (ondr...) qui recevra la récompense de bloc.
    #[arg(long)]
    address: String,

    /// Nombre de threads de minage (par défaut : tous les cœurs disponibles).
    #[arg(long)]
    threads: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Valide le format de l'adresse tout de suite pour échouer vite si elle est incorrecte.
    let _validated: Address = args.address.parse()?;

    let threads = args.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let client = reqwest::blocking::Client::new();

    tracing::info!(
        "mineur Ondris démarré : {threads} thread(s), node={}",
        args.node
    );

    let mut cached_dataset: Option<(u64, Arc<Dataset>)> = None;

    loop {
        let work: WorkTemplate = match client
            .get(format!("{}/work?miner={}", args.node, args.address))
            .send()
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => resp.json()?,
            Err(e) => {
                tracing::warn!("impossible de récupérer du travail depuis le node ({e}), nouvelle tentative dans 5s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        let dataset = match &cached_dataset {
            Some((epoch, ds)) if *epoch == work.epoch => ds.clone(),
            _ => {
                tracing::info!(
                    "génération du dataset local pour l'époque {} ({} Mio)...",
                    work.epoch,
                    ondris_pow::DATASET_SIZE / (1024 * 1024)
                );
                let seed = ondris_pow::epoch_seed(work.epoch_boundary_hash);
                let ds = Arc::new(Dataset::generate(work.epoch, seed));
                cached_dataset = Some((work.epoch, ds.clone()));
                ds
            }
        };

        tracing::info!(
            "minage du bloc {} (difficulté {})",
            work.block.header.height,
            work.block.header.difficulty
        );

        let mined = mine_block(work.block.clone(), work.target, dataset, threads);

        match client
            .post(format!("{}/block/submit", args.node))
            .json(&mined)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("bloc {} soumis avec succès !", mined.header.height);
            }
            Ok(resp) => {
                let body = resp.text().unwrap_or_default();
                tracing::warn!("bloc rejeté par le node: {body}");
            }
            Err(e) => tracing::warn!("échec d'envoi du bloc au node: {e}"),
        }
    }
}

/// Cherche un nonce satisfaisant `target` en répartissant l'espace des
/// nonces entre `threads` threads (thread i essaie i, i+threads, i+2*threads, ...).
fn mine_block(mut block: Block, target: [u8; 32], dataset: Arc<Dataset>, threads: usize) -> Block {
    let header_bytes = block.header.bytes_for_pow();
    let found = Arc::new(AtomicBool::new(false));
    let counter = Arc::new(AtomicU64::new(0));
    let result: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let start = Instant::now();

    std::thread::scope(|scope| {
        for t in 0..threads.max(1) {
            let found = found.clone();
            let counter = counter.clone();
            let result = result.clone();
            let dataset = dataset.clone();
            let header_bytes = header_bytes.clone();
            scope.spawn(move || {
                let mut nonce: u64 = t as u64;
                while !found.load(Ordering::Relaxed) {
                    let hash = ondris_pow::ondris_hash(&header_bytes, nonce, &dataset);
                    counter.fetch_add(1, Ordering::Relaxed);
                    if ondris_pow::meets_target(&hash, &target) {
                        *result.lock().unwrap() = Some(nonce);
                        found.store(true, Ordering::Relaxed);
                        break;
                    }
                    nonce = nonce.wrapping_add(threads.max(1) as u64);
                }
            });
        }

        while !found.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            if found.load(Ordering::Relaxed) {
                break;
            }
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed >= 4.5 {
                let hps = counter.load(Ordering::Relaxed) as f64 / elapsed;
                tracing::info!("hashrate: {:.1} H/s", hps);
            }
        }
    });

    let nonce = result
        .lock()
        .unwrap()
        .take()
        .expect("un thread doit avoir trouvé un nonce");
    block.header.nonce = nonce;
    block
}
