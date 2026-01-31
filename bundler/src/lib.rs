use actix_governor::{Governor, GovernorConfigBuilder};
use actix_web::{dev::Server, middleware::Logger, web, HttpServer};
use c_kzg::FIELD_ELEMENTS_PER_BLOB;
use clap::Parser;
use data_intent_tracker::DataIntentTracker;
use eth_provider::EthProvider;
use ethers::{
    signers::{coins_bip39::English, MnemonicBuilder, Signer},
    types::Address,
};
use eyre::{Context, Result};
use handlebars::Handlebars;
use sqlx::mysql::MySqlPoolOptions;
use std::{env, net::TcpListener, path::PathBuf, str::FromStr, sync::Arc, time::Duration};
use tokio::fs;
use url::Url;

use crate::{
    anchor_block::get_anchor_block,
    app::AppData,
    blob_sender_task::blob_sender_task,
    block_subscriber_task::block_subscriber_task,
    evict_stale_intents_task::evict_stale_intents_task,
    explorer::register_explorer_service,
    metrics::{get_metrics, push_metrics_task},
    remote_node_tracker_task::remote_node_tracker_task,
    routes::{
        delete_data::delete_data, get_balance_by_address, get_data, get_data_by_id, get_gas,
        get_health, get_sender, get_status_by_id, get_sync, post_data::post_data,
    },
    sync::{BlockSync, BlockSyncConfig},
    trusted_setup::TrustedSetup,
    utils::parse_basic_auth,
};

pub mod anchor_block;
mod app;
pub mod beacon_api_client;
mod blob_sender_task;
mod blob_tx_data;
mod block_subscriber_task;
pub mod consumer;
mod data_intent;
mod data_intent_tracker;
pub mod eth_provider;
mod evict_stale_intents_task;
mod explorer;
mod gas;
mod kzg;
mod metrics;
pub mod packing;
mod remote_node_tracker_task;
mod reth_fork;
mod routes;
mod sync;
mod trusted_setup;
pub mod utils;

pub use blob_tx_data::BlobTxSummary;
pub use data_intent::{BlobGasPrice, DataIntent};
pub use kzg::compute_blob_tx_hash;
pub use metrics::{PushMetricsConfig, PushMetricsFormat};

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
/// Max data allowed per user as pending data intents before inclusion
pub const MAX_PENDING_DATA_LEN_PER_USER: usize = MAX_USABLE_BLOB_DATA_LEN * 16;
/// Default target address
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

    /// Database URL to mysql DB with format `mysql://user:password@localhost/test`
    #[arg(env, long)]
    pub database_url: String,

    /// Maximum number of database connections in the pool
    #[arg(env, long, default_value_t = 10)]
    pub db_max_connections: u32,

    /// Remote node polling interval in seconds
    #[arg(env, long, default_value_t = 12)]
    pub node_poll_interval_sec: u64,

    /// Rate limit: requests per second per IP
    #[arg(env, long, default_value_t = 10)]
    pub rate_limit_per_second: u64,

    /// Rate limit: burst size per IP
    #[arg(env, long, default_value_t = 30)]
    pub rate_limit_burst: u32,

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

    /// Prune raw data from finalized intents after this many blocks past finalization.
    /// Set to 0 to disable pruning.
    #[arg(env, long, default_value_t = 1000)]
    pub prune_after_blocks: u64,

    /// Evict pending intents that have been underpriced for longer than this many hours.
    /// Set to 0 to disable eviction.
    #[arg(env, long, default_value_t = 24)]
    pub evict_stale_intent_hours: u64,
}

impl Args {
    pub fn address(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }
}

struct AppConfig {
    l1_inbox_address: Address,
    panic_on_background_task_errors: bool,
    metrics_server_bearer_token: Option<String>,
    metrics_push: Option<PushMetricsConfig>,
    node_poll_interval: Duration,
    /// Prune raw data from finalized intents after this many blocks past finalization.
    /// 0 means pruning is disabled.
    prune_after_blocks: u64,
    /// Evict pending intents underpriced for longer than this duration.
    /// Duration::ZERO means eviction is disabled.
    pub evict_stale_intent_duration: Duration,
}

pub struct App {
    port: u16,
    server: Server,
    metrics_server: Option<Server>,
    data: Arc<AppData>,
}

impl App {
    /// Instantiates components, fetching initial data, binds http server. Does not make progress
    /// on the server future. To actually run the app, call `Self::run`.
    pub async fn build(args: Args) -> Result<Self> {
        let mut provider = EthProvider::new(&args.eth_provider).await?;

        if let Some(eth_provider_interval) = args.eth_provider_interval {
            provider.set_interval(Duration::from_millis(eth_provider_interval));
        }

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

        let db_pool = MySqlPoolOptions::new()
            .max_connections(args.db_max_connections)
            .connect(&args.database_url)
            .await?;

        // TODO: choose starting point that's not genesis
        let anchor_block = get_anchor_block(&db_pool, &provider, args.starting_block).await?;
        debug!("retrieved anchor block: {:?}", anchor_block);

        let sync = BlockSync::new(
            BlockSyncConfig {
                target_address,
                finalize_depth: args.finalize_depth,
                max_pending_transactions: args.max_pending_transactions,
            },
            anchor_block,
        );
        // TODO: handle initial sync here with a nice progress bar

        let config = AppConfig {
            l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            panic_on_background_task_errors: args.panic_on_background_task_errors,
            node_poll_interval: Duration::from_secs(args.node_poll_interval_sec),
            prune_after_blocks: args.prune_after_blocks,
            evict_stale_intent_duration: Duration::from_secs(args.evict_stale_intent_hours * 3600),
            metrics_server_bearer_token: args.metrics_bearer_token.clone(),
            metrics_push: if let Some(url) = &args.metrics_push_url {
                Some(PushMetricsConfig {
                    url: Url::parse(url)
                        .wrap_err_with(|| format!("invalid push gateway URL {url}"))?,
                    basic_auth: if let Some(auth) = &args.metrics_push_basic_auth {
                        Some(parse_basic_auth(auth).wrap_err_with(|| "invalid push gateway auth")?)
                    } else {
                        None
                    },
                    interval: Duration::from_secs(args.metrics_push_interval_sec),
                    format: args.metrics_push_format,
                })
            } else {
                None
            },
        };

        // Handlebars uses a repository for the compiled templates. This object must be
        // shared between the application threads, and is therefore passed to the
        // Application Builder as an atomic reference-counted pointer.
        let mut handlebars = Handlebars::new();
        handlebars
            .register_templates_directory(
                "./static/templates",
                handlebars::DirectorySourceOptions {
                    tpl_extension: ".html".to_string(),
                    ..Default::default()
                },
            )
            .wrap_err("registering handlebars template")?;

        let app_data = Arc::new(AppData::new(
            handlebars,
            config,
            load_kzg_settings()?,
            db_pool,
            provider,
            wallet,
            chain_id,
            DataIntentTracker::default(),
            sync,
        ));

        info!(
            "connected to eth node at {} chain {}",
            &args.eth_provider, chain_id
        );

        info!("syncing data intent tracker");
        let synced_intents = app_data.sync_data_intents().await?;
        info!("synced data intent tracker, added {synced_intents} items");

        // Prints progress every few blocks to info level
        app_data.initial_block_sync().await?;

        let address = args.address();
        let listener = TcpListener::bind(address.clone())?;
        let listener_port = listener.local_addr()?.port();
        info!("Binding server on {}:{}", args.bind_address, listener_port);

        let (register_get_metrics, metrics_server) = if args.metrics {
            if args.metrics_port == args.port {
                info!("enabling metrics on server port");
                if args.metrics_bearer_token.is_none() {
                    warn!("UNSAFE: metrics exposed on the server port without auth");
                }
                (true, None)
            } else {
                let metrics_address = format!("{}:{}", args.bind_address, args.metrics_port);
                let metrics_listener = TcpListener::bind(&metrics_address)
                    .wrap_err_with(|| format!("binding metrics server on {metrics_address}"))?;
                info!("Binding metrics server on {}", metrics_address);

                let metrics_app_data = app_data.clone();
                let metrics_srv = HttpServer::new(move || {
                    actix_web::App::new()
                        .app_data(web::Data::new(metrics_app_data.clone()))
                        .service(get_metrics)
                })
                .listen(metrics_listener)?
                .run();

                (false, Some(metrics_srv))
            }
        } else {
            (false, None)
        };

        let governor_conf = GovernorConfigBuilder::default()
            .per_second(args.rate_limit_per_second)
            .burst_size(args.rate_limit_burst)
            .finish()
            .expect("invalid rate limit configuration");

        let app_data_clone = app_data.clone();
        let server = HttpServer::new(move || {
            // Limit JSON body to 256KB (enough for one blob + overhead)
            let json_cfg = web::JsonConfig::default().limit(256 * 1024);

            let app = actix_web::App::new()
                .wrap(Governor::new(&governor_conf))
                .wrap(Logger::default())
                .app_data(json_cfg)
                .app_data(web::Data::new(app_data_clone.clone()))
                .service(get_health)
                .service(get_sender)
                .service(get_sync)
                .service(get_gas)
                .service(post_data)
                .service(delete_data)
                .service(get_data)
                .service(get_data_by_id)
                .service(get_status_by_id)
                .service(get_balance_by_address);
            let app = register_explorer_service(app);

            // Conditionally register the metrics route
            if register_get_metrics {
                app.service(get_metrics)
            } else {
                app
            }
        })
        .listen(listener)?
        .run();

        Ok(App {
            port: listener_port,
            server,
            metrics_server,
            data: app_data,
        })
    }

    /// Long running future progressing server and background tasks futures
    pub async fn run(self) -> Result<()> {
        tokio::try_join!(
            run_server(self.server),
            run_optional_server(self.metrics_server),
            blob_sender_task(self.data.clone()),
            block_subscriber_task(self.data.clone()),
            remote_node_tracker_task(self.data.clone()),
            run_consistency_checks_task(self.data.clone()),
            push_metrics_task(self.data.config.metrics_push.clone()),
            evict_stale_intents_task(self.data.clone()),
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

async fn run_optional_server(server: Option<Server>) -> Result<()> {
    if let Some(server) = server {
        server.await?;
    }
    Ok(())
}

pub(crate) fn load_kzg_settings() -> Result<c_kzg::KzgSettings> {
    let trusted_setup: TrustedSetup = serde_json::from_reader(TRUSTED_SETUP_BYTES)?;
    Ok(c_kzg::KzgSettings::load_trusted_setup(
        &trusted_setup.g1_points(),
        &trusted_setup.g2_points(),
    )?)
}

async fn run_consistency_checks_task(data: Arc<AppData>) -> Result<()> {
    match data
        .initial_consistency_check_intents_with_inclusion_finalized()
        .await
    {
        Ok(ids_with_inclusion_finalized) => {
            info!("Completed initial consistency checks for intents with inclusion finalzied");
            debug!("ids_with_inclusion_finalized {ids_with_inclusion_finalized:?}",);
        }
        Err(e) => error!("Error doing initial consistency checks for intents {e:?}"),
    };
    Ok(())
}
