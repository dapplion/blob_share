use blob_share::{App, Args, PostDataIntent};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::Transaction,
    utils::{Anvil, AnvilInstance},
};
use eyre::Result;
use once_cell::sync::Lazy;
use std::{
    io::BufReader,
    process::{Child, Command},
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

pub const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

// Ensure that the `tracing` stack is only initialised once using `once_cell`
static TRACING: Lazy<()> = Lazy::new(|| {
    // a builder for `FmtSubscriber`.
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::TRACE)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
});

pub struct TestHarness {
    client: reqwest::Client,
    base_url: String,
    eth_provider: Provider<Http>,
    anvil: AnvilInstance,
}

impl TestHarness {
    pub async fn spawn() -> Self {
        Lazy::force(&TRACING);

        let anvil = Anvil::new().spawn();

        let args = Args {
            port: 0,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: anvil.endpoint(),
            // Set polling interval to 1 milisecond since anvil auto-mines on each transaction
            eth_provider_interval: Some(1),
            starting_block: 0,
        };

        let server = App::build(args).await.unwrap();

        let base_url = format!("http://127.0.0.1:{}", server.port());

        // Run app server in the background
        let _ = tokio::spawn(server.run());

        let eth_provider = Provider::<Http>::try_from(&anvil.endpoint()).unwrap();
        let eth_provider = eth_provider.interval(Duration::from_millis(1));

        Self {
            client: reqwest::Client::new(),
            base_url,
            eth_provider,
            anvil,
        }
    }

    // Test health of server
    // $ curl -vv localhost:8000/health
    pub async fn test_health(&self) {
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
    pub async fn post_data_intent(&self, from: &str, data: Vec<u8>, id: &str) {
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

    pub async fn get_data_intents(&self) -> Vec<PostDataIntent> {
        let response = self
            .client
            .get(&format!("{}/data", &self.base_url))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.json().await.unwrap()
    }

    pub async fn get_block_first_tx(&self, block_number: u64) -> Transaction {
        let block = self
            .eth_provider
            .get_block_with_txs(block_number)
            .await
            .unwrap()
            .expect("block should exist");
        block.transactions.get(0).unwrap().clone()
    }

    pub async fn get_eth_provider_version(&self) -> String {
        self.eth_provider.client_version().await.unwrap()
    }

    pub async fn wait_for_block(&self, block_number: u64, timeout_duration: Duration) {
        let start_time = Instant::now();

        while self.eth_provider.get_block_number().await.unwrap().as_u64() < block_number {
            if start_time.elapsed() > timeout_duration {
                let head = self.eth_provider.get_block_number().await.unwrap();
                panic!("Timeout reached waiting for block {block_number}, head number {head}");
            }

            sleep(Duration::from_millis(5)).await;
        }
    }
}
