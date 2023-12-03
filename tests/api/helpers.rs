use ethers::{
    providers::{Http, Middleware, Provider},
    signers::LocalWallet,
    types::{Address, Transaction},
};
use once_cell::sync::Lazy;
use std::{
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use blob_share::{
    client::{PostDataIntentV1, SenderDetails},
    App, Args, Client, DataIntent,
};

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
    pub client: Client,
    base_url: String,
    eth_provider: Provider<Http>,
}

impl TestHarness {
    pub async fn spawn() -> Self {
        Lazy::force(&TRACING);

        let args = Args {
            port: 0,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: "http://localhost:8545".to_string(),
            // Set polling interval to 1 milisecond since anvil auto-mines on each transaction
            eth_provider_interval: Some(1),
            starting_block: 0,
            mnemonic:
                "work man father plunge mystery proud hollow address reunion sauce theory bonus"
                    .to_string(),
        };

        let server = App::build(args).await.unwrap();

        let base_url = format!("http://127.0.0.1:{}", server.port());

        // Run app server in the background
        let _ = tokio::spawn(server.run());

        let eth_provider = Provider::<Http>::try_from("http://localhost:8545").unwrap();
        let eth_provider = eth_provider.interval(Duration::from_millis(50));

        Self {
            client: Client::new(&base_url),
            base_url,
            eth_provider,
        }
    }

    pub fn sender_address(&self) -> Address {
        Address::from_str(ADDRESS_ZERO).unwrap()
    }

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    pub async fn post_data(&self, wallet: &LocalWallet, data: Vec<u8>, id: &str) {
        let max_cost_wei = 10000;

        self.client
            .post_data(
                &DataIntent::with_signature(wallet, data, max_cost_wei)
                    .await
                    .unwrap()
                    .into(),
            )
            .await
            .unwrap();
        log::info!("posted data intent: {}", id);
    }

    pub async fn get_data(&self) -> Vec<PostDataIntentV1> {
        self.client.get_data().await.unwrap()
    }

    pub async fn get_sender(&self) -> SenderDetails {
        self.client.get_sender().await.unwrap()
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

    pub async fn wait_for_block_with_tx_from(
        &self,
        from: Address,
        timeout: Duration,
        interval: Option<Duration>,
    ) -> Transaction {
        let interval = interval.unwrap_or(Duration::from_millis(5));
        let start_time = Instant::now();
        let mut last_block_number = 0;

        loop {
            let block_number = self.eth_provider.get_block_number().await.unwrap().as_u64();
            if last_block_number > block_number {
                last_block_number = block_number;

                let block = self
                    .eth_provider
                    .get_block_with_txs(block_number)
                    .await
                    .unwrap()
                    .expect("block should be available");

                for tx in block.transactions {
                    if tx.from == from {
                        return tx;
                    }
                }
            }

            if start_time.elapsed() > timeout {
                let head = self.eth_provider.get_block_number().await.unwrap();
                panic!("Timeout reached waiting for block {block_number}, head number {head}");
            }

            sleep(interval).await;
        }
    }
}
