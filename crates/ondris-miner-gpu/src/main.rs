//! GPU miner for Ondris (OpenCL). See docs/ALGORITHM.md and
//! docs/ARCHITECTURE.md in the repo root for the algorithm this ports.
//!
//! Run `ondris-miner-gpu self-test` first on any new GPU/driver — it
//! checks the kernel's output against the CPU reference
//! (`ondris_pow::ondris_hash_with_sizes`) at both tiny and full-size
//! parameters before anything is ever mined for real. Do not skip this;
//! it's the whole point of the validation chain described in
//! `blake3_ref.rs`.

use clap::{Parser, Subcommand};
use ondris_core::{Block, WorkTemplate};
use ondris_miner_gpu::gpu::Gpu;
use ondris_pow::Dataset;
use ondris_primitives::{Address, Hash256};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "ondris-miner-gpu",
    version,
    about = "OpenCL GPU miner for Ondris (testnet)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validates the OpenCL kernel against the CPU reference implementation.
    /// Run this before mining on any new GPU or driver version.
    SelfTest,
    /// Mines against a node's RPC using the GPU.
    Mine {
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        node: String,
        /// Address (ondr...) that will receive the block reward.
        #[arg(long)]
        address: String,
        /// Nonces tried per kernel launch. Each one needs its own
        /// `SCRATCHPAD_SIZE`-byte slice of a single GPU buffer, so this is
        /// bounded both by total VRAM and by the driver's max single
        /// allocation size (often well under total VRAM — e.g. 2048 at
        /// the default 2 MiB scratchpad already hit
        /// CL_INVALID_BUFFER_SIZE on a 12 GB RTX 4070 Super in testing;
        /// 512 is a safe starting point to raise from).
        #[arg(long, default_value_t = 512)]
        batch_size: usize,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    match args.command {
        Command::SelfTest => self_test(),
        Command::Mine {
            node,
            address,
            batch_size,
        } => mine(node, address, batch_size),
    }
}

fn self_test() -> anyhow::Result<()> {
    tracing::info!("initializing OpenCL...");
    let gpu = Gpu::new()?;
    tracing::info!("using device: {}", gpu.device_name);

    tracing::info!("checking tiny sizes (scratchpad=4096B, dataset=8192B, 3 rounds)...");
    let seed = Hash256::hash(b"gpu-self-test-seed");
    let dataset = Dataset::generate_with_sizes(0, seed, 4096, 8192);
    for header in [&b"header-a"[..], b"a different, longer header value"] {
        for nonce in [0u64, 1, 42, u64::MAX, 123456789] {
            let expected = ondris_pow::ondris_hash_with_sizes(header, nonce, &dataset, 4096, 3);
            let got = gpu.hash_debug(dataset.bytes(), header, nonce, 4096, 3)?;
            anyhow::ensure!(
                got == *expected.as_bytes(),
                "MISMATCH at tiny size for header={header:?} nonce={nonce}\n  cpu: {}\n  gpu: {}",
                hex::encode(expected.as_bytes()),
                hex::encode(&got)
            );
            tracing::info!(
                "  header={:?} nonce={nonce}: OK",
                String::from_utf8_lossy(header)
            );
        }
    }

    tracing::info!(
        "checking full default sizes (scratchpad={}B, dataset={}B, {} rounds) — this exercises the multi-chunk BLAKE3 path...",
        ondris_pow::SCRATCHPAD_SIZE,
        ondris_pow::DATASET_SIZE,
        ondris_pow::MIX_ROUNDS
    );
    let seed2 = Hash256::hash(b"gpu-self-test-full-size-seed");
    let start = Instant::now();
    let full_dataset =
        Dataset::generate_with_sizes(0, seed2, ondris_pow::CACHE_SIZE, ondris_pow::DATASET_SIZE);
    tracing::info!("dataset generated in {:.2}s", start.elapsed().as_secs_f64());

    let header = b"a realistic length header value for this check";
    let nonce = 424242u64;
    let expected = ondris_pow::ondris_hash(header, nonce, &full_dataset);
    let t0 = Instant::now();
    let got = gpu.hash_debug(
        full_dataset.bytes(),
        header,
        nonce,
        ondris_pow::SCRATCHPAD_SIZE as u32,
        ondris_pow::MIX_ROUNDS as u32,
    )?;
    let gpu_time = t0.elapsed();
    anyhow::ensure!(
        got == *expected.as_bytes(),
        "MISMATCH at full default size for nonce={nonce}\n  cpu: {}\n  gpu: {}",
        hex::encode(expected.as_bytes()),
        hex::encode(&got)
    );
    tracing::info!(
        "full-size single-hash check: OK (one GPU hash took {:.1}ms)",
        gpu_time.as_secs_f64() * 1000.0
    );

    tracing::info!("ALL CHECKS PASSED — the kernel reproduces the CPU reference exactly.");
    Ok(())
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn mine(node: String, address: String, batch_size: usize) -> anyhow::Result<()> {
    let miner_address: Address = address.parse()?;
    tracing::info!("initializing OpenCL...");
    let gpu = Gpu::new()?;
    tracing::info!("using device: {}, batch size {batch_size}", gpu.device_name);

    let client = reqwest::blocking::Client::new();
    let mut cached_dataset: Option<(u64, Arc<Dataset>)> = None;
    // Kept alive across blocks (and re-created only when the epoch — and
    // therefore the dataset — changes): re-uploading a multi-ten-MB
    // dataset and reallocating a `batch_size * SCRATCHPAD_SIZE` pool on
    // every single block, let alone every batch, dwarfed the actual
    // hashing cost in early testing.
    let mut session: Option<(u64, ondris_miner_gpu::gpu::MiningSession<'_>)> = None;

    loop {
        let work: WorkTemplate = match client
            .get(format!("{node}/work?miner={miner_address}"))
            .send()
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => resp.json()?,
            Err(e) => {
                tracing::warn!("could not fetch work from the node ({e}), retrying in 5s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        let dataset = match &cached_dataset {
            Some((epoch, ds)) if *epoch == work.epoch => ds.clone(),
            _ => {
                tracing::info!("generating local dataset for epoch {}...", work.epoch);
                let seed = ondris_pow::epoch_seed(work.epoch_boundary_hash);
                let ds = Arc::new(Dataset::generate(work.epoch, seed));
                cached_dataset = Some((work.epoch, ds.clone()));
                session = None; // dataset changed, the old session's upload is stale
                ds
            }
        };

        if session.is_none() {
            tracing::info!("uploading dataset to the GPU for epoch {}...", work.epoch);
            let new_session = gpu.mining_session(
                dataset.bytes(),
                ondris_pow::SCRATCHPAD_SIZE as u32,
                batch_size,
            )?;
            session = Some((work.epoch, new_session));
        }
        let (_, session_ref) = session.as_mut().expect("just set above if it was None");

        tracing::info!(
            "mining block {} (difficulty {}) on GPU, batch {batch_size}",
            work.block.header.height,
            work.block.header.difficulty
        );

        let mined = mine_block_gpu(
            session_ref,
            work.block.clone(),
            work.target,
            &dataset,
            batch_size,
        )?;

        match client
            .post(format!("{node}/block/submit"))
            .json(&mined)
            .send()
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("block {} submitted successfully!", mined.header.height);
            }
            Ok(resp) => {
                let body = resp.text().unwrap_or_default();
                tracing::warn!("block rejected by the node: {body}");
            }
            Err(e) => tracing::warn!("failed to send block to the node: {e}"),
        }
    }
}

fn mine_block_gpu(
    session: &mut ondris_miner_gpu::gpu::MiningSession<'_>,
    mut block: Block,
    target: [u8; 32],
    dataset: &Dataset,
    batch_size: usize,
) -> anyhow::Result<Block> {
    let header_bytes = block.header.bytes_for_pow();
    let mut nonce_base = 0u64;
    let start = Instant::now();
    let mut hashes_done: u64 = 0;

    loop {
        let found = session.try_batch(
            &header_bytes,
            nonce_base,
            ondris_pow::MIX_ROUNDS as u32,
            &target,
        )?;
        hashes_done += batch_size as u64;

        if let Some(nonce) = found {
            // Never trust the GPU's own comparison for something this
            // consequential — re-check on the CPU with the exact same
            // reference function the node will use to verify the block.
            let confirmed_hash = ondris_pow::ondris_hash(&header_bytes, nonce, dataset);
            if ondris_pow::meets_target(&confirmed_hash, &target) {
                block.header.nonce = nonce;
                return Ok(block);
            }
            tracing::warn!(
                "GPU reported nonce {nonce} as a hit but the CPU re-check disagrees — treating as a false positive and continuing"
            );
        }

        nonce_base += batch_size as u64;
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed > 0.0 && hashes_done.is_multiple_of(batch_size as u64 * 10) {
            tracing::info!("hashrate: {:.1} H/s", hashes_done as f64 / elapsed);
        }
    }
}
