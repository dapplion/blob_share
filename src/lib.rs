use actix_web::{dev::Server, middleware::Logger, web, HttpServer};
use c_kzg::FIELD_ELEMENTS_PER_BLOB;
use clap::Parser;
use ethers::{
    providers::{Http, Middleware, Provider},
    signers::{coins_bip39::English, LocalWallet, MnemonicBuilder},
    types::Address,
    utils::keccak256,
};
use eyre::Result;
use std::{
    collections::HashMap,
    net::TcpListener,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

use crate::{
    gas::GasConfig,
    kzg::{construct_blob_tx, send_blob_tx},
    routes::{get_data, get_status, post_data},
    trusted_setup::TrustedSetup,
};

mod blob_tx_data;
mod gas;
mod kzg;
mod routes;
mod sync;
mod trusted_setup;
mod tx_eip4844;
mod tx_sidecar;

pub use routes::PostDataIntent;

/// Current encoding needs one byte per field element
pub const MAX_USABLE_BLOB_DATA_LEN: usize = 31 * FIELD_ELEMENTS_PER_BLOB;
const MIN_BLOB_DATA_TO_PUBLISH: usize = (80 * MAX_USABLE_BLOB_DATA_LEN) / 100; // 80%
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
}

impl Args {
    pub fn address(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }
}

#[derive(Clone)]
struct DataIntent {
    from: String,
    data: Vec<u8>,
    max_price: u64,
    data_hash: DataHash,
}

type DataHash = [u8; 32];
type DataIntentId = (String, DataHash);

type DataIntents = HashMap<DataIntentId, DataIntent>;

struct PublishConfig {
    l1_inbox_address: Address,
}

struct AppData {
    kzg_settings: c_kzg::KzgSettings,
    queue: Mutex<DataIntents>,
    provider: Provider<Http>,
    sender_wallet: LocalWallet,
    publish_config: PublishConfig,
    notify: Notify,
}

fn compute_data_hash(data: &[u8]) -> DataHash {
    keccak256(data)
}

async fn verify_account_balance(_account: &str) -> bool {
    // TODO: Check account solvency with offchain or on-chain system
    true
}

async fn blob_sender_task(app_data: Arc<AppData>) {
    loop {
        app_data.notify.notified().await;

        let wei_per_byte = 1; // TODO: Fetch from chain

        let next_blob_items = {
            let items = app_data.queue.lock().unwrap();
            select_next_blob_items(&items, wei_per_byte)
        };

        if let Some(next_blob_items) = next_blob_items {
            log::info!(
                "selected items for blob tx, count {}",
                next_blob_items.len()
            );

            // Clear items from pool
            {
                let mut items = app_data.queue.lock().unwrap();
                for item in next_blob_items.iter() {
                    items.remove(&(item.from.clone(), item.data_hash));
                }
            }

            let gas_config = GasConfig::estimate(&app_data.provider)
                .await
                .expect("TODO: handle fetch error");

            let blob_tx = construct_blob_tx(
                &app_data.kzg_settings,
                &app_data.publish_config,
                &gas_config,
                &app_data.sender_wallet,
                next_blob_items,
            )
            .expect("TODO: handle bad data");
            // TODO: do not await here, spawn another task
            // TODO: monitor transaction, if gas is insufficient return data intents to the pool
            send_blob_tx(&app_data.provider, blob_tx)
                .await
                .unwrap_or_else(|e| {
                    log::error!("error sending blob transaction {:?}", e);
                });
        } else {
            log::info!("no viable set of items for blob");
        }
    }
}

// TODO: write optimizer algo to find a better distribution
// TODO: is ok to represent wei units as usize?
fn select_next_blob_items(items: &DataIntents, _wei_per_byte: usize) -> Option<Vec<DataIntent>> {
    // Sort items by data length
    let mut items = items.values().collect::<Vec<_>>();
    items.sort_by(|a, b| b.data.len().cmp(&a.data.len()));

    let mut intents_for_blob: Vec<&DataIntent> = vec![];
    let mut used_len = 0;

    for item in items {
        if used_len + item.data.len() > MAX_USABLE_BLOB_DATA_LEN {
            break;
        }

        used_len += item.data.len();
        intents_for_blob.push(item);
    }

    if used_len < MIN_BLOB_DATA_TO_PUBLISH {
        return None;
    }

    return Some(intents_for_blob.into_iter().cloned().collect());
}

pub struct App {
    server: Server,
    port: u16,
}

impl App {
    pub async fn build(args: Args) -> Result<Self> {
        let provider = Provider::<Http>::try_from(&args.eth_provider)?;

        // Pass interval option
        let provider = if let Some(interval) = args.eth_provider_interval {
            provider.interval(Duration::from_millis(interval))
        } else {
            provider
        };

        // TODO: read as param
        let phrase =
            "work man father plunge mystery proud hollow address reunion sauce theory bonus";
        // Child key at derivation path: m/44'/60'/0'/0/{index}
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(phrase)
            .index(0u32)?
            .build()?;

        // TODO: Load properly
        let trusted_setup: TrustedSetup = serde_json::from_reader(TRUSTED_SETUP_BYTES)?;
        let kzg_settings = c_kzg::KzgSettings::load_trusted_setup(
            &trusted_setup.g1_points(),
            &trusted_setup.g2_points(),
        )?;

        let app_data = Arc::new(AppData {
            kzg_settings,
            notify: <_>::default(),
            queue: <_>::default(),
            publish_config: PublishConfig {
                l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            },
            provider,
            sender_wallet: wallet,
        });

        let eth_client_version = app_data.provider.client_version().await?;
        let eth_net_version = app_data.provider.client_version().await?;
        log::info!(
            "connected to eth node at {} version {} chain {}",
            &args.eth_provider,
            eth_client_version,
            eth_net_version
        );

        let data_clone = app_data.clone();
        tokio::spawn(async move {
            blob_sender_task(data_clone).await;
        });

        let address = args.address();
        let listener = TcpListener::bind(address.clone())?;
        let listener_port = listener.local_addr().unwrap().port();
        log::info!("Starting server on {}:{}", args.bind_address, listener_port);

        let server = HttpServer::new(move || {
            actix_web::App::new()
                .wrap(Logger::default())
                .app_data(web::Data::new(app_data.clone()))
                .service(get_status)
                .service(post_data)
                .service(get_data)
        })
        .listen(listener)?
        .run();

        Ok(App {
            server,
            port: listener_port,
        })
    }

    pub async fn run(self) -> Result<(), std::io::Error> {
        self.server.await
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}
