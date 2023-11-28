use actix_web::{dev::Server, middleware::Logger, web, App, HttpServer};
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

mod gas;
mod kzg;
mod routes;
mod trusted_setup;
mod tx_eip4844;
mod tx_sidecar;

/// Current encoding needs one byte per field element
const MAX_USABLE_BLOB_DATA_LEN: usize = 31 * FIELD_ELEMENTS_PER_BLOB;
const MIN_BLOB_DATA_TO_PUBLISH: usize = (80 * MAX_USABLE_BLOB_DATA_LEN) / 100; // 80%
const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

pub const TRUSTED_SETUP_BYTES: &[u8] = include_bytes!("../trusted_setup.json");

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Name of the person to greet
    #[arg(short, long, default_value_t = 5000)]
    port: u16,

    /// Number of times to greet
    #[arg(short, long, default_value = "127.0.0.1")]
    bind_address: String,

    /// JSON RPC endpoint for an ethereum execution node
    #[arg(long)]
    eth_provider: String,

    /// JSON RPC polling interval in miliseconds, used for testing
    #[arg(long)]
    eth_provider_interval: Option<u64>,
}

impl Args {
    fn address(&self) -> String {
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

#[actix_web::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let server = run_app(args).await?;
    server.await?;
    Ok(())
}

async fn run_app(args: Args) -> Result<Server> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let provider = Provider::<Http>::try_from(&args.eth_provider)?;

    // Pass interval option
    let provider = if let Some(interval) = args.eth_provider_interval {
        provider.interval(Duration::from_millis(interval))
    } else {
        provider
    };

    // TODO: read as param
    let phrase = "work man father plunge mystery proud hollow address reunion sauce theory bonus";
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

    let address = args.address();
    log::info!("Starting server on {}", address);

    let data_clone = app_data.clone();
    tokio::spawn(async move {
        blob_sender_task(data_clone).await;
    });

    let server = HttpServer::new(move || {
        App::new()
            .wrap(Logger::default())
            .app_data(web::Data::new(app_data.clone()))
            .service(get_status)
            .service(post_data)
            .service(get_data)
    })
    .bind(address)?
    .run();

    Ok(server)
}

#[cfg(test)]
mod tests {
    use ethers::{core::utils::Anvil, types::Transaction};
    use eyre::Result;
    use std::time::Duration;
    use tokio::time::sleep;

    use crate::{routes::PostDataIntent, *};

    #[tokio::test]
    async fn run_blob_sharing_service() -> Result<()> {
        let anvil = Anvil::new().spawn();

        let args = Args {
            port: 8000,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: anvil.endpoint(),
            // Set polling interval to 1 milisecond since anvil auto-mines on each transaction
            eth_provider_interval: Some(1),
        };
        let base_url = format!("http://{}", args.address());

        let server = run_app(args).await?;
        let _ = tokio::spawn(server);

        let testing_harness = TestingHarness::new(base_url, anvil.endpoint());

        testing_harness.test_health().await;

        // monitor chain
        print_state(anvil.endpoint()).await;

        // Submit data intent
        let from = "0x0000000000000000000000000000000000000000".to_string();
        let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 2];
        let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];

        // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
        testing_harness
            .post_data_intent(&from, data_1.clone(), "data_intent_1")
            .await;

        // Check data intent is stored
        let intents = testing_harness.get_data_intents().await;
        assert_eq!(intents.len(), 1);

        testing_harness
            .post_data_intent(&from, data_2.clone(), "data_intent_2")
            .await;

        sleep(Duration::from_millis(100)).await;

        // After sending enough intents the blob transaction should be emitted
        let intents = testing_harness.get_data_intents().await;
        assert_eq!(intents.len(), 0);

        testing_harness.wait_for_block(1).await;
        let tx = testing_harness.get_block_first_tx(1).await;
        let expected_data = [data_1, data_2].concat();
        // Assert correctly constructed transaction
        assert_eq!(hex::encode(&tx.input), hex::encode(expected_data));
        assert_eq!(tx.to, Some(Address::from_str(ADDRESS_ZERO).unwrap()));

        Ok(())
    }

    struct TestingHarness {
        client: reqwest::Client,
        base_url: String,
        eth_provider: Provider<Http>,
    }

    impl TestingHarness {
        fn new(api_base_url: String, eth_provider_url: String) -> Self {
            let provider = Provider::<Http>::try_from(&eth_provider_url).unwrap();
            let provider = provider.interval(Duration::from_millis(1));

            Self {
                client: reqwest::Client::new(),
                base_url: api_base_url,
                eth_provider: provider,
            }
        }

        // Test health of server
        // $ curl -vv localhost:8000/health
        async fn test_health(&self) {
            let response = self
                .client
                .get(&format!("{}/health", &self.base_url))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            log::info!("health ok");
        }

        // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
        async fn post_data_intent(&self, from: &str, data: Vec<u8>, id: &str) {
            let response = self
                .client
                .post(&format!("{}/data", &self.base_url))
                .json(&PostDataIntent {
                    from: from.into(),
                    data,
                    max_price: 1,
                })
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            log::info!("posted data intent: {}", id);
        }

        async fn get_data_intents(&self) -> Vec<PostDataIntent> {
            let response = self
                .client
                .get(&format!("{}/data", &self.base_url))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            response.json().await.unwrap()
        }

        async fn get_block_first_tx(&self, block_number: u64) -> Transaction {
            let block = self
                .eth_provider
                .get_block_with_txs(block_number)
                .await
                .unwrap()
                .expect("block should exist");
            block.transactions.get(0).unwrap().clone()
        }

        async fn wait_for_block(&self, block_number: u64) {
            while self.eth_provider.get_block_number().await.unwrap().as_u64() < block_number {
                sleep(Duration::from_millis(5)).await;
            }
        }
    }

    async fn print_state(url: String) {
        let provider = Provider::<Http>::try_from(&url).unwrap();
        let block_num = provider.get_block_number().await.unwrap();
        log::info!("block number {}", block_num);
    }
}
