use actix_web::{dev::Server, middleware::Logger, web, HttpServer};
use c_kzg::FIELD_ELEMENTS_PER_BLOB;
use clap::Parser;
use data_intent_tracker::DataIntentTracker;
use eth_provider::EthProvider;
use ethers::{
    signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
    types::Address,
};
use eyre::{bail, eyre, Context, Result};
use std::{
    collections::HashMap, env, io, net::TcpListener, path::PathBuf, str::FromStr, sync::Arc,
    time::Duration,
};
use tokio::{
    fs,
    sync::{Notify, RwLock},
};
use url::Url;

use crate::{
    blob_sender_task::blob_sender_task,
    block_subscriber_task::block_subscriber_task,
    metrics::{get_metrics, push_metrics_task},
    routes::{
        get_balance_by_address, get_data, get_data_by_id, get_health, get_home,
        get_last_seen_nonce_by_address, get_sender, get_status_by_id, get_sync,
        post_data::post_data,
    },
    sync::{AnchorBlock, BlockSync, BlockSyncConfig},
    trusted_setup::TrustedSetup,
    utils::parse_basic_auth,
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
mod metrics;
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
pub use metrics::{PushMetricsConfig, PushMetricsFormat};
pub use utils::increase_by_min_percent;

// Use log crate when building application
#[cfg(not(test))]
pub(crate) use tracing::{debug, error, info, warn};

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
    #[arg(env, short, long, default_value_t = 5000)]
    pub port: u16,

    /// Number of times to greet
    #[arg(env, short, long, default_value = "127.0.0.1")]
    pub bind_address: String,

    /// JSON RPC endpoint for an ethereum execution node
    #[arg(env, long, default_value = "ws://127.0.0.1:8546")]
    pub eth_provider: String,

    /// JSON RPC polling interval in miliseconds, used for testing
    #[arg(env, long)]
    pub eth_provider_interval: Option<u64>,

    /// First block for service to start accounting
    #[arg(env, long, default_value_t = 0)]
    pub starting_block: u64,

    /// Directory to persist anchor block finalized data
    #[arg(env, long, default_value = "./data")]
    pub data_dir: String,

    /// Mnemonic for tx sender. If not set a random account will be generated.
    /// TODO: UNSAFE, handle hot keys better
    #[arg(env, long)]
    pub mnemonic: Option<String>,

    /// FOR TESTING ONLY: panic if a background task experiences an error for a single event
    #[arg(env, long)]
    pub panic_on_background_task_errors: bool,

    /// Consider blocks `finalize_depth` behind current head final. If there's a re-org deeper than
    /// this depth, the app will crash and expect to re-sync on restart.
    #[arg(env, long, default_value_t = 64)]
    pub finalize_depth: u64,

    /// Max count of pending transactions that will be sent before waiting for inclusion of the
    /// previously sent transactions. A number higher than the max count of blobs per block should
    /// not result better UX. However, a higher number risks creating transactions that can become
    /// underpriced in volatile network conditions.
    #[arg(env, long, default_value_t = 6)]
    pub max_pending_transactions: u64,

    /// Enable serving metrics
    #[arg(env, long)]
    pub metrics: bool,
    /// Metrics server port. If it's the same as the main server it will be served there
    #[arg(env, long, default_value_t = 9000)]
    pub metrics_port: u16,
    /// Require callers to the /metrics endpoint to add Bearer token auth
    #[arg(env, long)]
    pub metrics_bearer_token: Option<String>,

    /// Enable prometheus push gateway to the specified URL
    #[arg(env, long)]
    pub metrics_push_url: Option<String>,
    /// Customize push gateway frequency
    #[arg(env, long, default_value_t = 15)]
    pub metrics_push_interval_sec: u64,
    /// Provide Basic Auth for push gateway requests
    #[arg(env, long)]
    pub metrics_push_basic_auth: Option<String>,
    /// Format to send push gateway metrics
    #[arg(env, long, value_enum, default_value_t = PushMetricsFormat::Protobuf)]
    pub metrics_push_format: PushMetricsFormat,
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
    anchor_block_filepath: PathBuf,
    metrics_server_bearer_token: Option<String>,
    metrics_push: Option<PushMetricsConfig>,
}

struct AppData {
    kzg_settings: c_kzg::KzgSettings,
    data_intent_tracker: RwLock<DataIntentTracker>,
    // TODO: Store in remote DB persisting
    sign_nonce_tracker: RwLock<HashMap<Address, u128>>,
    sync: RwLock<BlockSync>,
    provider: EthProvider,
    sender_wallet: LocalWallet,
    publish_config: PublishConfig,
    notify: Notify,
    chain_id: u64,
    config: AppConfig,
}

impl AppData {
    async fn collect_metrics(&self) {
        self.sync.read().await.collect_metrics();
        self.data_intent_tracker.read().await.collect_metrics();
    }
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

        let provider = EthProvider::new(&args.eth_provider).await?;
        let chain_id = provider.get_chainid().await?.as_u64();

        // TODO: read as param
        // Child key at derivation path: m/44'/60'/0'/0/{index}
        let wallet = match args.mnemonic {
            Some(ref mnemonic) => MnemonicBuilder::<English>::default()
                .phrase(mnemonic.as_str())
                .build()?,
            None => {
                let mut rng = rand::thread_rng();
                let dir = env::current_dir()?;
                warn!(
                    "USING RANDONMLY GENERATED MNEMONIC, persisted in {}",
                    dir.to_string_lossy()
                );
                MnemonicBuilder::<English>::default()
                    .write_to(dir)
                    .build_random(&mut rng)?
            }
        }
        .with_chain_id(chain_id);
        // Address to send funds to increase an account's balance
        let target_address = wallet.address();

        // Ensure data_dir exists
        let data_dir = PathBuf::from(&args.data_dir);
        fs::create_dir_all(&data_dir)
            .await
            .wrap_err_with(|| "creating data dir")?;

        let anchor_block_filepath = data_dir.join("anchor_block.json");

        // TODO: choose starting point that's not genesis
        let anchor_block = {
            // Attempt to read persisted file first if exists
            match fs::read_to_string(&anchor_block_filepath).await {
                Ok(str) => {
                    serde_json::from_str(&str).wrap_err_with(|| "parsing anchor block file")?
                }
                Err(e) => match e.kind() {
                    io::ErrorKind::NotFound => match starting_point {
                        StartingPoint::StartingBlock(starting_block) => {
                            anchor_block_from_starting_block(&provider, starting_block).await?
                        }
                    },
                    _ => bail!(
                        "error opening anchor_block file {}: {e:?}",
                        anchor_block_filepath.to_string_lossy()
                    ),
                },
            }
        };

        let sync = BlockSync::new(
            BlockSyncConfig {
                target_address,
                finalize_depth: args.finalize_depth,
                max_pending_transactions: args.max_pending_transactions,
            },
            anchor_block,
        );

        let app_data = Arc::new(AppData {
            kzg_settings: load_kzg_settings()?,
            notify: <_>::default(),
            data_intent_tracker: <_>::default(),
            sign_nonce_tracker: <_>::default(),
            sync: sync.into(),
            publish_config: PublishConfig {
                l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            },
            provider,
            sender_wallet: wallet,
            chain_id,
            config: AppConfig {
                panic_on_background_task_errors: args.panic_on_background_task_errors,
                anchor_block_filepath,
                metrics_server_bearer_token: args.metrics_bearer_token.clone(),
                metrics_push: if let Some(url) = &args.metrics_push_url {
                    Some(PushMetricsConfig {
                        url: Url::parse(url)
                            .wrap_err_with(|| format!("invalid push gateway URL {url}"))?,
                        basic_auth: if let Some(auth) = &args.metrics_push_basic_auth {
                            Some(
                                parse_basic_auth(auth)
                                    .wrap_err_with(|| "invalid push gateway auth")?,
                            )
                        } else {
                            None
                        },
                        interval: Duration::from_secs(args.metrics_push_interval_sec),
                        format: args.metrics_push_format,
                    })
                } else {
                    None
                },
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
            let app = actix_web::App::new()
                .wrap(Logger::default())
                .app_data(web::Data::new(app_data_clone.clone()))
                .service(get_home)
                .service(get_health)
                .service(get_sender)
                .service(get_sync)
                .service(post_data)
                .service(get_data)
                .service(get_data_by_id)
                .service(get_status_by_id)
                .service(get_balance_by_address)
                .service(get_last_seen_nonce_by_address);

            // Conditionally register the metrics route
            if args.metrics && args.metrics_port == args.port {
                info!("enabling metrics on server port");
                if args.metrics_bearer_token.is_none() {
                    warn!("UNSAFE: metrics exposed on the server port without auth");
                }
                app.service(get_metrics)
            } else {
                app
            }
        })
        .listen(listener)?
        .run();

        // TODO: serve metrics on different port
        if args.metrics && args.metrics_port != args.port {
            todo!("serve metrics on different port");
        }

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
            push_metrics_task(self.data.config.metrics_push.clone()),
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

/// Initialize empty anchor block state from a network block
async fn anchor_block_from_starting_block(
    provider: &EthProvider,
    starting_block: u64,
) -> Result<AnchorBlock> {
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
    Ok(AnchorBlock {
        hash,
        number,
        gas: BlockGasSummary::from_block(&anchor_block)?,
        // At genesis all balances are zero
        finalized_balances: <_>::default(),
    })
}
