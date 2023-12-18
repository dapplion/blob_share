use actix_web::{dev::Server, middleware::Logger, web, HttpServer};
use c_kzg::FIELD_ELEMENTS_PER_BLOB;
use clap::Parser;
use data_intent_tracker::DataIntentTracker;
use eth_provider::EthProvider;
use ethers::{
    signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
    types::Address,
};
use eyre::{eyre, Context, Result};
use std::{net::TcpListener, str::FromStr, sync::Arc};
use tokio::sync::{Notify, RwLock};
use url::Url;

use crate::{
    blob_sender_task::blob_sender_task,
    block_subscriber_task::block_subscriber_task,
    routes::{
        get_balance_by_address, get_data, get_data_by_id, get_health, get_home, get_sender,
        get_status_by_id, post_data,
    },
    sync::{AnchorBlock, BlockSync},
    trusted_setup::TrustedSetup,
};

pub mod beacon_api_client;
mod blob_sender_task;
mod blob_tx_data;
mod block_subscriber_task;
pub mod client;
pub mod consumer;
mod data_intent;
mod data_intent_tracker;
mod eth_provider;
mod gas;
mod kzg;
pub mod packing;
mod reth_fork;
mod routes;
mod sync;
mod trusted_setup;
mod utils;

pub use blob_tx_data::BlobTxSummary;
pub use client::Client;
pub use data_intent::DataIntent;
pub use gas::BlockGasSummary;
pub use utils::increase_by_min_percent;

// Use log crate when building application
#[cfg(not(test))]
pub(crate) use log::{debug, error, info, warn};

// Workaround to use prinltn! for logs.
// std stdio has dedicated logic to capture logs during test execution
// https://github.com/rust-lang/rust/blob/1fdfe1234795a289af1088aefa92ef80191cb611/library/std/src/io/stdio.rs#L18
#[cfg(test)]
pub(crate) use std::{println as error, println as warn, println as info, println as debug};

/// Current encoding needs one byte per field element
pub const MAX_USABLE_BLOB_DATA_LEN: usize = 31 * FIELD_ELEMENTS_PER_BLOB;
const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

pub const TRUSTED_SETUP_BYTES: &[u8] = include_bytes!("../trusted_setup.json");

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Name of the person to greet
    #[arg(short, long, default_value_t = 5000)]
    pub port: u16,

    /// Number of times to greet
    #[arg(short, long, default_value = "127.0.0.1")]
    pub bind_address: String,

    /// JSON RPC endpoint for an ethereum execution node
    #[arg(long, default_value = "ws://127.0.0.1:8546")]
    pub eth_provider: String,

    /// JSON RPC polling interval in miliseconds, used for testing
    #[arg(long)]
    pub eth_provider_interval: Option<u64>,

    /// First block for service to start accounting
    #[arg(long, default_value_t = 0)]
    pub starting_block: u64,

    /// Mnemonic for tx sender
    /// TODO: UNSAFE, handle hot keys better
    #[arg(
        long,
        default_value = "any any any any any any any any any any any any"
    )]
    pub mnemonic: String,

    /// FOR TESTING ONLY: panic if a background task experiences an error for a single event
    #[arg(long)]
    pub panic_on_background_task_errors: bool,

    /// Consider blocks `finalize_depth` behind current head final. If there's a re-org deeper than
    /// this depth, the app will crash and expect to re-sync on restart.
    #[arg(long, default_value_t = 64)]
    pub finalize_depth: u64,
}

impl Args {
    pub fn address(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }
}

struct PublishConfig {
    pub(crate) l1_inbox_address: Address,
}

struct AppConfig {
    panic_on_background_task_errors: bool,
}

struct AppData {
    kzg_settings: c_kzg::KzgSettings,
    data_intent_tracker: RwLock<DataIntentTracker>,
    sync: RwLock<BlockSync>,
    provider: EthProvider,
    sender_wallet: LocalWallet,
    publish_config: PublishConfig,
    notify: Notify,
    chain_id: u64,
    config: AppConfig,
}

pub struct App {
    port: u16,
    server: Server,
    data: Arc<AppData>,
}

enum StartingPoint {
    StartingBlock(u64),
}

impl App {
    /// Instantiates components, fetching initial data, binds http server. Does not make progress
    /// on the server future. To actually run the app, call `Self::run`.
    pub async fn build(args: Args) -> Result<Self> {
        let starting_point = StartingPoint::StartingBlock(args.starting_block);

        let provider = match Url::parse(&args.eth_provider)
            .wrap_err_with(|| format!("invalid eth_provider URL {}", args.eth_provider))?
            .scheme()
        {
            "ws" | "wss" => EthProvider::new_ws(&args.eth_provider)
                .await
                .wrap_err_with(|| {
                    format!("unable to connect to WS eth provider {}", args.eth_provider)
                })?,
            _ => EthProvider::new_http(&args.eth_provider)?,
        };

        let chain_id = provider.get_chainid().await?.as_u64();

        // TODO: read as param
        // Child key at derivation path: m/44'/60'/0'/0/{index}
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(args.mnemonic.as_str())
            .index(0u32)?
            .build()?
            .with_chain_id(chain_id);
        // Address to send funds to increase an account's balance
        let target_address = wallet.address();

        // TODO: choose starting point that's not genesis
        let anchor_block = match starting_point {
            StartingPoint::StartingBlock(starting_block) => {
                let anchor_block = provider
                    .get_block(starting_block)
                    .await?
                    .ok_or_else(|| eyre!("genesis block not available"))?;
                let hash = anchor_block
                    .hash
                    .ok_or_else(|| eyre!("block has no hash property"))?;
                let number = anchor_block
                    .number
                    .ok_or_else(|| eyre!("block has no number property"))?
                    .as_u64();
                let target_address_nonce = provider
                    .get_transaction_count(target_address, Some(hash.into()))
                    .await?
                    .as_u64();
                AnchorBlock {
                    hash,
                    number,
                    target_address_nonce,
                    gas: BlockGasSummary::from_block(&anchor_block)?,
                    // At genesis all balances are zero
                    finalized_balances: <_>::default(),
                }
            }
        };

        let sync = BlockSync::new(target_address, args.finalize_depth, anchor_block);

        let app_data = Arc::new(AppData {
            kzg_settings: load_kzg_settings()?,
            notify: <_>::default(),
            data_intent_tracker: <_>::default(),
            sync: sync.into(),
            publish_config: PublishConfig {
                l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            },
            provider,
            sender_wallet: wallet,
            chain_id,
            config: AppConfig {
                panic_on_background_task_errors: args.panic_on_background_task_errors,
            },
        });

        info!(
            "connected to eth node at {} chain {}",
            &args.eth_provider, chain_id
        );

        let address = args.address();
        let listener = TcpListener::bind(address.clone())?;
        let listener_port = listener.local_addr().unwrap().port();
        info!("Binding server on {}:{}", args.bind_address, listener_port);

        let app_data_clone = app_data.clone();
        let server = HttpServer::new(move || {
            actix_web::App::new()
                .wrap(Logger::default())
                .app_data(web::Data::new(app_data_clone.clone()))
                .service(get_home)
                .service(get_health)
                .service(get_sender)
                .service(post_data)
                .service(get_data)
                .service(get_data_by_id)
                .service(get_status_by_id)
                .service(get_balance_by_address)
        })
        .listen(listener)?
        .run();

        Ok(App {
            port: listener_port,
            server,
            data: app_data,
        })
    }

    /// Long running future progressing server and background tasks futures
    pub async fn run(self) -> Result<()> {
        tokio::try_join!(
            run_server(self.server),
            blob_sender_task(self.data.clone()),
            block_subscriber_task(self.data.clone()),
        )?;
        Ok(())
    }

    pub fn sender_address(&self) -> Address {
        self.data.sender_wallet.address()
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

pub async fn run_server(server: Server) -> Result<()> {
    Ok(server.await?)
}

pub(crate) fn load_kzg_settings() -> Result<c_kzg::KzgSettings> {
    let trusted_setup: TrustedSetup = serde_json::from_reader(TRUSTED_SETUP_BYTES)?;
    Ok(c_kzg::KzgSettings::load_trusted_setup(
        &trusted_setup.g1_points(),
        &trusted_setup.g2_points(),
    )?)
}
