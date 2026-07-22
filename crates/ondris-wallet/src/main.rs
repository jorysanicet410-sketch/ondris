mod keystore;

use clap::{Parser, Subcommand};
use ondris_core::{AccountInfo, SubmitTxResponse, Transaction};
use ondris_primitives::Address;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ondris-wallet",
    version,
    about = "Wallet CLI pour Ondris (testnet)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crée un nouveau wallet chiffré.
    New {
        #[arg(long)]
        out: PathBuf,
        /// Si omis, le mot de passe est demandé de façon interactive.
        #[arg(long)]
        password: Option<String>,
    },
    /// Affiche l'adresse d'un wallet existant (pas besoin du mot de passe).
    Address {
        #[arg(long)]
        wallet: PathBuf,
    },
    /// Interroge le node pour le solde et le nonce d'un wallet.
    Balance {
        #[arg(long)]
        wallet: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        node: String,
    },
    /// Signe et envoie une transaction via le node.
    Send {
        #[arg(long)]
        wallet: PathBuf,
        #[arg(long)]
        password: Option<String>,
        /// Adresse destinataire (ondr...).
        #[arg(long)]
        to: String,
        /// Montant en plus petite unité (1 ONDR = 100_000_000 unités).
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 0)]
        fee: u64,
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        node: String,
    },
}

fn get_password(provided: Option<String>) -> anyhow::Result<String> {
    match provided {
        Some(p) => Ok(p),
        None => Ok(rpassword::prompt_password("Mot de passe du wallet: ")?),
    }
}

fn fetch_account(
    client: &reqwest::blocking::Client,
    node: &str,
    address: &str,
) -> anyhow::Result<AccountInfo> {
    let info = client
        .get(format!("{node}/account/{address}"))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(info)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    match args.command {
        Command::New { out, password } => {
            let password = get_password(password)?;
            anyhow::ensure!(
                password.len() >= 8,
                "le mot de passe doit faire au moins 8 caractères"
            );
            let (ks, keypair) = keystore::create(&password)?;
            keystore::save(&out, &ks)?;
            println!("Wallet créé : {}", out.display());
            println!("Adresse     : {}", keypair.address());
            println!("⚠ Sauvegarde ce fichier ET ton mot de passe : sans les deux, les fonds sont perdus.");
        }
        Command::Address { wallet } => {
            let ks = keystore::load(&wallet)?;
            println!("{}", ks.address);
        }
        Command::Balance { wallet, node } => {
            let ks = keystore::load(&wallet)?;
            let client = reqwest::blocking::Client::new();
            let info = fetch_account(&client, &node, &ks.address)?;
            println!("Adresse : {}", info.address);
            println!("Solde   : {} (plus petite unité)", info.balance);
            println!("Nonce   : {}", info.nonce);
        }
        Command::Send {
            wallet,
            password,
            to,
            amount,
            fee,
            node,
        } => {
            let ks = keystore::load(&wallet)?;
            let password = get_password(password)?;
            let keypair = keystore::unlock(&ks, &password)?;

            let client = reqwest::blocking::Client::new();
            let info = fetch_account(&client, &node, &ks.address)?;
            let to_addr: Address = to.parse()?;

            let mut tx =
                Transaction::new_unsigned(keypair.public(), to_addr, amount, fee, info.nonce);
            tx.sign(&keypair);

            let resp: SubmitTxResponse = client
                .post(format!("{node}/tx/submit"))
                .json(&tx)
                .send()?
                .error_for_status()?
                .json()?;
            println!("Transaction envoyée : {}", resp.tx_hash);
        }
    }

    Ok(())
}
