use ethers::{
    providers::{Http, Middleware, Provider},
    signers::LocalWallet,
    types::{Address, Transaction, TransactionRequest, H256},
    utils::parse_ether,
};
use eyre::{bail, Result};
use once_cell::sync::Lazy;
use std::{
    future::Future,
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use blob_share::{
    client::{DataIntentId, DataIntentStatus, PostDataIntentV1, PostDataResponse, SenderDetails},
    App, Args, Client, DataIntent,
};

use crate::{
    geth_helpers::{get_wallet_genesis_funds, GethMode, WalletWithProvider},
    run_lodestar::{spawn_lodestar, LodestarInstance, RunLodestarArgs},
    spawn_geth, GethInstance,
};

pub const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

// Ensure that the `tracing` stack is only initialised once using `once_cell`
static TRACING: Lazy<()> = Lazy::new(|| {
    // a builder for `FmtSubscriber`.
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::INFO)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
});

#[allow(dead_code)]
pub struct TestHarness {
    pub client: Client,
    base_url: String,
    eth_provider: Provider<Http>,
    geth_instance: GethInstance,
    lodestar_instance: Option<LodestarInstance>,
}

pub enum TestMode {
    ELOnly,
    WithChain,
}

impl TestHarness {
    pub async fn spawn_with_el_only() -> Self {
        TestHarness::spawn(TestMode::ELOnly).await
    }

    pub async fn spawn_with_chain() -> Self {
        TestHarness::spawn(TestMode::WithChain).await
    }

    pub async fn spawn(test_mode: TestMode) -> Self {
        Lazy::force(&TRACING);

        let (geth_instance, lodestar_instance) = match test_mode {
            TestMode::ELOnly => {
                let geth = spawn_geth(GethMode::Dev).await;
                (geth, None)
            }
            TestMode::WithChain => {
                let geth = spawn_geth(GethMode::Interop).await;
                let lodestar = spawn_lodestar(RunLodestarArgs {
                    execution_url: geth.authrpc_url(true),
                    genesis_eth1_hash: geth.genesis_block_hash_hex(),
                })
                .await;
                (geth, Some(lodestar))
            }
        };

        let args = Args {
            port: 0,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: geth_instance.ws_url().to_string(),
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

        let eth_provider = Provider::<Http>::try_from(geth_instance.http_url()).unwrap();
        let eth_provider = eth_provider.interval(Duration::from_millis(50));

        Self {
            client: Client::new(&base_url),
            base_url,
            eth_provider,
            geth_instance,
            lodestar_instance,
        }
    }

    pub fn sender_address(&self) -> Address {
        Address::from_str(ADDRESS_ZERO).unwrap()
    }

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    pub async fn post_data(&self, wallet: &LocalWallet, data: Vec<u8>) -> DataIntentId {
        let max_cost_wei = 10000;

        let res = self
            .client
            .post_data(
                &DataIntent::with_signature(wallet, data, max_cost_wei)
                    .await
                    .unwrap()
                    .into(),
            )
            .await
            .unwrap();
        DataIntentId::from_str(&res.id).unwrap()
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

    pub async fn wait_for_pending_intent(
        &self,
        id: &DataIntentId,
        timeout: Duration,
    ) -> Result<H256> {
        let start_time = Instant::now();

        loop {
            match self.client.get_status_by_id(&id.to_string()).await? {
                None => bail!("data intent 1 unknown {:?}", id),
                Some(DataIntentStatus::Pending) => {}
                Some(DataIntentStatus::InPendingTx { tx_hash }) => return Ok(tx_hash),
                Some(DataIntentStatus::InConfirmedTx { tx_hash, .. }) => return Ok(tx_hash),
            }

            if start_time.elapsed() > timeout {
                bail!("Timeout {:?}", timeout)
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn fund_sender_account(&self, wallet: &WalletWithProvider) {
        let sender = self.client.get_sender().await.unwrap();

        let tx = TransactionRequest::new()
            .from(wallet.address())
            .to(sender.address)
            .value(parse_ether("0.1").unwrap());
        let tx = wallet.send_transaction(tx, None).await.unwrap();
        tx.confirmations(1).await.unwrap();
    }

    pub fn get_wallet_genesis_funds(&self) -> WalletWithProvider {
        self.geth_instance.http_provider().unwrap()
    }
}
