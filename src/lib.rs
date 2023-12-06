use actix_web::{dev::Server, middleware::Logger, web, HttpServer};
use c_kzg::FIELD_ELEMENTS_PER_BLOB;
use clap::Parser;
use data_intent_tracker::DataIntentTracker;
use ethers::{
    providers::{Middleware, Provider, Ws},
    signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
    types::Address,
};
use eyre::{eyre, Result};
use std::{net::TcpListener, str::FromStr, sync::Arc, time::Duration};
use tokio::sync::Notify;

use crate::{
    blob_sender_task::blob_sender_task,
    block_subscriber_task::block_subscriber_task,
    gas::GasTracker,
    routes::{get_data, get_data_by_id, get_health, get_sender, get_status_by_id, post_data},
    sync::BlockSync,
    trusted_setup::TrustedSetup,
};

mod blob_sender_task;
mod blob_tx_data;
mod block_subscriber_task;
pub mod client;
mod data_intent;
mod data_intent_tracker;
mod gas;
mod kzg;
mod routes;
mod sync;
mod trusted_setup;
mod tx_eip4844;
mod tx_sidecar;
mod utils;

pub use client::Client;
pub use data_intent::DataIntent;

/// Current encoding needs one byte per field element
pub const MAX_USABLE_BLOB_DATA_LEN: usize = 31 * FIELD_ELEMENTS_PER_BLOB;
const MIN_BLOB_DATA_TO_PUBLISH: usize = MAX_USABLE_BLOB_DATA_LEN / 2; // 50%
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
    #[arg(long)]
    pub eth_provider: String,

    /// JSON RPC polling interval in miliseconds, used for testing
    #[arg(long)]
    pub eth_provider_interval: Option<u64>,

    /// First block for service to start accounting
    #[arg(long, default_value_t = 0)]
    pub starting_block: u64,

    /// Mnemonic for tx sender
    /// TODO: UNSAFE, handle hot keys better
    #[arg(long)]
    pub mnemonic: String,
}

impl Args {
    pub fn address(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }
}

struct PublishConfig {
    pub(crate) l1_inbox_address: Address,
}

struct AppData {
    kzg_settings: c_kzg::KzgSettings,
    data_intent_tracker: DataIntentTracker,
    sync: BlockSync,
    gas_tracker: GasTracker,
    provider: Provider<Ws>,
    sender_wallet: LocalWallet,
    publish_config: PublishConfig,
    notify: Notify,
    chain_id: u64,
}

pub struct App {
    port: u16,
    server: Server,
    data: Arc<AppData>,
}

enum StartingPoint {
    Genesis,
}

impl App {
    /// Instantiates components, fetching initial data, binds http server. Does not make progress
    /// on the server future. To actually run the app, call `Self::run`.
    pub async fn build(args: Args) -> Result<Self> {
        let starting_point = StartingPoint::Genesis;

        let provider = Provider::<Ws>::connect(&args.eth_provider).await?;

        // Pass interval option
        let provider = if let Some(interval) = args.eth_provider_interval {
            provider.interval(Duration::from_millis(interval))
        } else {
            provider
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
        let (anchor_block_root, anchor_block_number) = match starting_point {
            StartingPoint::Genesis => {
                let anchor_block = provider
                    .get_block(0)
                    .await?
                    .ok_or_else(|| eyre!("genesis block not available"))?;
                (
                    anchor_block
                        .hash
                        .ok_or_else(|| eyre!("block has no hash property"))?,
                    anchor_block
                        .number
                        .ok_or_else(|| eyre!("block has no number property"))?
                        .as_u64(),
                )
            }
        };
        let sync = BlockSync::new(target_address, anchor_block_root, anchor_block_number);

        // Initialize gas tracker with current head
        let head_number = provider.get_block_number().await?;
        let current_head = provider
            .get_block(head_number)
            .await?
            .ok_or_else(|| eyre!("head block not available {head_number}"))?;
        let gas_tracker = GasTracker::new(&current_head)?;

        let app_data = Arc::new(AppData {
            kzg_settings: load_kzg_settings()?,
            notify: <_>::default(),
            data_intent_tracker: <_>::default(),
            sync,
            gas_tracker,
            publish_config: PublishConfig {
                l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            },
            provider,
            sender_wallet: wallet,
            chain_id,
        });

        let eth_client_version = app_data.provider.client_version().await?;
        let eth_net_version = app_data.provider.client_version().await?;
        log::info!(
            "connected to eth node at {} version {} chain {}",
            &args.eth_provider,
            eth_client_version,
            eth_net_version
        );

        let address = args.address();
        let listener = TcpListener::bind(address.clone())?;
        let listener_port = listener.local_addr().unwrap().port();
        log::info!("Binding server on {}:{}", args.bind_address, listener_port);

        let app_data_clone = app_data.clone();
        let server = HttpServer::new(move || {
            actix_web::App::new()
                .wrap(Logger::default())
                .app_data(web::Data::new(app_data_clone.clone()))
                .service(get_health)
                .service(get_sender)
                .service(post_data)
                .service(get_data)
                .service(get_data_by_id)
                .service(get_status_by_id)
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
