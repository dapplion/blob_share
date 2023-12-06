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
    mem,
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use blob_share::{
    client::{DataIntentId, DataIntentStatus, SenderDetails},
    App, Args, Client, DataIntent,
};

use crate::{
    geth_helpers::{GethMode, WalletWithProvider},
    run_lodestar::{spawn_lodestar, LodestarInstance, RunLodestarArgs},
    spawn_geth, GethInstance,
};

pub const ADDRESS_ZERO: &str = "0x0000000000000000000000000000000000000000";

pub const SEC_1: Duration = Duration::from_secs(1);
pub const MS_100: Duration = Duration::from_millis(100);

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
    pub eth_provider: Provider<Http>,
    geth_instance: GethInstance,
    lodestar_instance: Option<LodestarInstance>,
    pub sender_address: Address,
    app_status: AppStatus,
}

pub enum TestMode {
    ELOnly,
    WithChain,
}

pub enum AppStatus {
    Built(App),
    Running,
}

impl TestHarness {
    pub async fn spawn_with_el_only() -> Self {
        TestHarness::build(TestMode::ELOnly)
            .await
            .spawn_app_in_background()
    }

    pub async fn spawn_with_chain() -> Self {
        TestHarness::build(TestMode::WithChain)
            .await
            .spawn_app_in_background()
    }

    pub async fn build(test_mode: TestMode) -> Self {
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
            panic_on_background_task_errors: true,
        };

        let app = App::build(args).await.unwrap();

        let base_url = format!("http://127.0.0.1:{}", app.port());
        let sender_address = app.sender_address();

        let eth_provider = Provider::<Http>::try_from(geth_instance.http_url()).unwrap();
        let eth_provider = eth_provider.interval(Duration::from_millis(50));

        let client = Client::new(&base_url);

        Self {
            client,
            base_url,
            eth_provider,
            geth_instance,
            lodestar_instance,
            sender_address,
            app_status: AppStatus::Built(app),
        }
    }

    pub fn spawn_app_in_background(mut self) -> Self {
        match mem::replace(&mut self.app_status, AppStatus::Running) {
            AppStatus::Running => panic!("app already running"),
            AppStatus::Built(app) => {
                // Run app server in the background
                let _ = tokio::spawn(app.run());
            }
        }
        self
    }

    pub async fn spawn_with_fn<F, Fut>(mut self, f: F) -> Result<()>
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let app = match mem::replace(&mut self.app_status, AppStatus::Running) {
            AppStatus::Running => panic!("app already running"),
            AppStatus::Built(app) => app,
        };

        let app_future = app.run();
        let f_future = f(self);

        tokio::select! {
            result = app_future => {
                // This branch is executed if app.run() finishes first
                // this should never happen as the app future never stop unless in case of an error
                // return the result to propagate the error early
                return result;
            }
            result = f_future => {
                // This branch is executed if f() finishes first
                // f() can finish by completing the test successfully, or by encountering some error
                return result;
            }
        }
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

    pub async fn wait_for_intent_inclusion_in_any_block(
        &self,
        id: &DataIntentId,
        expected_tx_hash: Option<H256>,
        timeout: Duration,
    ) -> Result<(H256, H256)> {
        let start_time = Instant::now();

        loop {
            let status = self.client.get_status_by_id(&id.to_string()).await?;
            match status {
                DataIntentStatus::Unknown
                | DataIntentStatus::Pending
                | DataIntentStatus::InPendingTx { .. } => {} // fall through
                DataIntentStatus::InConfirmedTx {
                    tx_hash,
                    block_hash,
                } => return Ok((tx_hash, block_hash)),
            }

            if start_time.elapsed() > timeout {
                let sender_nonce = self
                    .eth_provider
                    .get_transaction_count(self.sender_address, None)
                    .await?
                    .as_usize();
                let sender_txs = if sender_nonce > 0 {
                    self.get_all_past_sender_txs()
                        .await?
                        .iter()
                        .map(|tx| format!("tx {:?} block {}", tx.hash, tx.block_number.unwrap()))
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };
                bail!(
                    "Timeout {timeout:?}: waiting for intent {id} inclusion in block, \
                expected tx_hash {expected_tx_hash:?} \
                current status {status:?}, sender nonce {sender_nonce} sender txs {sender_txs:?}",
                )
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn wait_for_intent_inclusion_in_any_tx(
        &self,
        id: &DataIntentId,
        timeout: Duration,
    ) -> Result<H256> {
        let start_time = Instant::now();

        loop {
            match self.client.get_status_by_id(&id.to_string()).await? {
                DataIntentStatus::Unknown | DataIntentStatus::Pending => {} // fall through
                DataIntentStatus::InPendingTx { tx_hash } => return Ok(tx_hash),
                DataIntentStatus::InConfirmedTx { tx_hash, .. } => return Ok(tx_hash),
            }

            if start_time.elapsed() > timeout {
                bail!("Timeout {timeout:?}: waiting for intent {id} inclusion in tx")
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn wait_for_known_intent(&self, id: &DataIntentId, timeout: Duration) -> Result<()> {
        let start_time = Instant::now();

        loop {
            match self.client.get_status_by_id(&id.to_string()).await? {
                DataIntentStatus::InConfirmedTx { .. }
                | DataIntentStatus::InPendingTx { .. }
                | DataIntentStatus::Pending => return Ok(()),
                DataIntentStatus::Unknown => {
                    // fall through
                    if start_time.elapsed() > timeout {
                        let data_intents = self.client.get_data().await?;
                        let ids = data_intents.iter().map(|d| d.id()).collect::<Vec<_>>();
                        bail!(
                            "timeout {:?}: intent {:?} not known: intents known: {:?}",
                            timeout,
                            id,
                            ids
                        );
                    }
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn get_all_past_sender_txs(&self) -> Result<Vec<Transaction>> {
        let mut txs = vec![];
        let head_number = self.eth_provider.get_block_number().await?.as_u64();
        for block_number in (1..=head_number).rev() {
            let block = self
                .eth_provider
                .get_block_with_txs(block_number)
                .await?
                .expect(&format!("no block at number {block_number}"));
            for tx in block.transactions {
                if tx.from == self.sender_address {
                    txs.push(tx);
                }
            }
        }
        Ok(txs)
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