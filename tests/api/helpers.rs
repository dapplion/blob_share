use ethers::{
    providers::{Http, Middleware, Provider},
    signers::LocalWallet,
    types::{Address, Transaction, TransactionRequest, H256},
    utils::parse_ether,
};
use eyre::{bail, eyre, Context, Result};
use futures::future::try_join_all;
use log::LevelFilter;
use std::{
    future::Future,
    mem,
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};

use blob_share::{
    client::DataIntentId, consumer::BlobConsumer, App, Args, BlockGasSummary, Client, DataIntent,
};

use crate::{
    geth_helpers::{GethMode, WalletWithProvider},
    run_lodestar::{spawn_lodestar, LodestarInstance, RunLodestarArgs},
    spawn_geth, GethInstance,
};

pub const ONE_HUNDRED_MS: Duration = Duration::from_millis(100);

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

    #[allow(dead_code)]
    pub async fn spawn_with_chain() -> Self {
        TestHarness::build(TestMode::WithChain)
            .await
            .spawn_app_in_background()
    }

    pub async fn build(test_mode: TestMode) -> Self {
        //  Lazy::force(&TRACING);

        // From env_logger docs to capture logs in tests
        // REF: https://docs.rs/env_logger/latest/env_logger/#capturing-logs-in-tests
        let _ = env_logger::builder()
            .format_timestamp_millis()
            // Enabling debug for everything causes hyper and reqwest to spam logs
            .filter(None, LevelFilter::Info)
            // Enable debug for this crate
            .filter(Some("blob_share"), LevelFilter::Debug)
            .is_test(true)
            .try_init();

        let (geth_instance, lodestar_instance) = match test_mode {
            TestMode::ELOnly => {
                let geth = spawn_geth(GethMode::Dev).await;
                (geth, None)
            }
            TestMode::WithChain => {
                let geth = spawn_geth(GethMode::Interop).await;
                let lodestar = spawn_lodestar(RunLodestarArgs {
                    execution_url: geth.authrpc_url(),
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

        let client = Client::new(&base_url).unwrap();

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

    pub fn get_blob_consumer(&self, participant_address: Address) -> BlobConsumer {
        let beacon_api_base_url = match &self.lodestar_instance {
            Some(lodestar_instance) => lodestar_instance.http_url(),
            None => panic!("no beacon node is configured"),
        };

        BlobConsumer::new(
            beacon_api_base_url,
            self.geth_instance.http_url(),
            self.sender_address,
            participant_address,
        )
        .unwrap()
    }

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    pub async fn post_data(&self, wallet: &LocalWallet, data: Vec<u8>) -> DataIntentId {
        // Choose data pricing correctly
        let head_block_number = self.eth_provider.get_block_number().await.unwrap();
        let head_block = self
            .eth_provider
            .get_block(head_block_number)
            .await
            .unwrap()
            .expect("head block should exist");
        let blob_gas_price_next_block = BlockGasSummary::from_block(&head_block)
            .unwrap()
            .blob_gas_price_next_block();

        // TODO: customize, for now set gas price equal to next block
        // TODO: Close to genesis block the value is 1, which requires blobs to be perfectly full
        let max_cost_wei = blob_gas_price_next_block;

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

    pub async fn post_data_and_wait_for_pending(
        &self,
        wallet: &LocalWallet,
        data: Vec<u8>,
    ) -> DataIntentId {
        let intent_id = self.post_data(wallet, data).await;
        self.wait_for_known_intents(&[intent_id], ONE_HUNDRED_MS)
            .await
            .unwrap();
        intent_id
    }

    pub async fn wait_for_intent_inclusion_in_any_block(
        &self,
        id: &DataIntentId,
        expected_tx_hash: Option<H256>,
        timeout: Duration,
    ) -> Result<(H256, H256)> {
        match retry_with_timeout(
            || async {
                let status = self.client.get_status_by_id(*id).await?;
                Ok(status
                    .is_in_block()
                    .ok_or_else(|| eyre!("still pending: {status:?}"))?)
            },
            timeout,
            ONE_HUNDRED_MS,
        )
        .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
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
                    "Timeout {timeout:?}: waiting for intent {id} inclusion in block: {e:?}, \
                expected tx_hash {expected_tx_hash:?} \
                sender nonce {sender_nonce} sender txs {sender_txs:?}",
                )
            }
        }
    }

    pub async fn wait_for_intent_inclusion_in_any_tx(
        &self,
        ids: &[DataIntentId],
        timeout: Duration,
    ) -> Result<Vec<H256>> {
        match retry_with_timeout(
            || async {
                let mut tx_hashes = vec![];
                for id in ids {
                    match self.client.get_status_by_id(*id).await?.is_in_tx() {
                        Some(tx_hash) => tx_hashes.push(tx_hash),
                        None => bail!("still pending"),
                    }
                }
                Ok(tx_hashes)
            },
            timeout,
            ONE_HUNDRED_MS,
        )
        .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                let statuses = try_join_all(
                    ids.iter()
                        .map(|id| self.client.get_status_by_id(*id))
                        .collect::<Vec<_>>(),
                )
                .await?;
                bail!(
                    "timeout {timeout:?}: waiting for inclusion of intents {ids:?}: {e:?}\ncurrent status: {statuses:?}",
                );
            }
        }
    }

    pub async fn wait_for_known_intents(
        &self,
        ids: &[DataIntentId],
        timeout: Duration,
    ) -> Result<()> {
        match retry_with_timeout(
            || async {
                let statuses = try_join_all(
                    ids.iter()
                        .map(|id| self.client.get_status_by_id(*id))
                        .collect::<Vec<_>>(),
                )
                .await?;

                if statuses.iter().map(|status| status.is_known()).all(|b| b) {
                    Ok(())
                } else {
                    bail!("some still pending {statuses:?}")
                }
            },
            timeout,
            ONE_HUNDRED_MS,
        )
        .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                let data_intents = self.client.get_data().await?;
                let known_ids = data_intents.iter().map(|d| d.id()).collect::<Vec<_>>();
                bail!(
                    "timeout {timeout:?}: waiting for intents {ids:?} not known: {e:?}\nintents known: {known_ids:?}",
                );
            }
        }
    }

    pub async fn wait_for_app_health(&self, timeout: Duration) -> Result<()> {
        retry_with_timeout(
            || async { self.client.health().await },
            timeout,
            Duration::from_millis(10),
        )
        .await
        .wrap_err("timeout waiting for health")
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
        timeout(Duration::from_secs(30), tx.confirmations(1))
            .await
            .unwrap()
            .unwrap();
    }

    pub fn get_wallet_genesis_funds(&self) -> WalletWithProvider {
        self.geth_instance.http_provider().unwrap()
    }
}

pub async fn retry_with_timeout<T, Fut, F: FnMut() -> Fut>(
    mut f: F,
    timeout: Duration,
    retry_interval: Duration,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    let start = Instant::now();
    loop {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if start.elapsed() > timeout {
                    return Err(e.wrap_err(format!("timeout {timeout:?}")));
                } else {
                    sleep(retry_interval).await;
                }
            }
        }
    }
}
