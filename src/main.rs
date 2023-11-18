use actix_web::{dev::Server, middleware::Logger, web, App, HttpServer};
use clap::Parser;
use ethers::{
    providers::{Http, Middleware, Provider},
    types::TransactionRequest,
    utils::keccak256,
};
use eyre::Result;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

use crate::routes::{get_data, get_status, post_data};

mod routes;

const MAX_BLOB_DATA_LEN: usize = 131072; // 32 * 4096;
const MIN_BLOB_DATA_TO_PUBLISH: usize = 104857; // 80%
const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

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

struct BlobTx {
    data: Vec<u8>,
}

struct PublishConfig {
    l1_inbox_address: String,
}

struct AppData {
    queue: Mutex<DataIntents>,
    provider: Provider<Http>,
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

            let blob_tx = construct_blob_tx(next_blob_items);
            // TODO: do not await here, spawn another task
            // TODO: monitor transaction, if gas is insufficient return data intents to the pool
            send_blob_tx(&app_data.provider, &app_data.publish_config, blob_tx)
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
    let mut intents_for_blob: Vec<&DataIntent> = vec![];
    let mut used_len = 0;

    for item in items.values() {
        if used_len + item.data.len() > MAX_BLOB_DATA_LEN {
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

fn construct_blob_tx(next_blob_items: Vec<DataIntent>) -> BlobTx {
    let mut data = vec![];
    for mut item in next_blob_items.into_iter() {
        data.append(&mut item.data);
    }

    BlobTx { data }
}

async fn send_blob_tx(
    provider: &Provider<Http>,
    publish_config: &PublishConfig,
    blob_tx: BlobTx,
) -> Result<()> {
    let accounts = provider.get_accounts().await?;
    let from = accounts[0];

    // craft the tx
    let tx = TransactionRequest::new()
        .to(&publish_config.l1_inbox_address)
        .value(0)
        .data(blob_tx.data)
        .from(from);

    // broadcast it via the eth_sendTransaction API
    let tx = provider.send_transaction(tx, None).await?.await?;

    log::info!("Sent blob transaction {}", serde_json::to_string(&tx)?);

    Ok(())
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

    let app_data = Arc::new(AppData {
        notify: <_>::default(),
        queue: <_>::default(),
        publish_config: PublishConfig {
            l1_inbox_address: ADDRESS_ZERO.to_string(),
        },
        provider: Provider::<Http>::try_from(&args.eth_provider)?,
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
    use ethers::core::utils::Anvil;
    use eyre::Result;
    use std::time::Duration;
    use tokio::time::sleep;

    use crate::{routes::PostDataIntent, *};

    async fn post_data_intent(base_url: &str, from: &str, data: Vec<u8>, id: &str) {
        // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
        let response = reqwest::Client::new()
            .post(&format!("{}/data", &base_url))
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

    async fn get_data_intents(base_url: &str) -> Vec<PostDataIntent> {
        let response = reqwest::Client::new()
            .get(&format!("{}/data", &base_url))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.json().await.unwrap()
    }

    #[tokio::test]
    async fn run_blob_sharing_service() -> Result<()> {
        let anvil = Anvil::new().spawn();

        let args = Args {
            port: 8000,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: anvil.endpoint(),
        };
        let base_url = format!("http://{}", args.address());

        let server = run_app(args).await?;
        let _ = tokio::spawn(server);

        // Test health of server
        // $ curl -vv localhost:8000/health
        let response = reqwest::Client::new()
            .get(&format!("{}/health", &base_url))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        log::info!("health ok");

        // Submit data intent
        let from = "0x0000000000000000000000000000000000000000".to_string();
        let data_1 = vec![0xaa_u8; MAX_BLOB_DATA_LEN / 2];
        let data_2 = vec![0xbb_u8; MAX_BLOB_DATA_LEN / 2];

        // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
        post_data_intent(&base_url, &from, data_1, "data_intent_1").await;

        // Check data intent is stored
        let intents = get_data_intents(&base_url).await;
        assert_eq!(intents.len(), 1);

        post_data_intent(&base_url, &from, data_2, "data_intent_2").await;

        sleep(Duration::from_millis(100)).await;

        // After sending enough intents the blob transaction should be emitted
        let intents = get_data_intents(&base_url).await;
        assert_eq!(intents.len(), 0);

        Ok(())
    }
}
