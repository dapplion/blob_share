use ethers::{
    providers::{Http, Middleware, Provider},
    signers::LocalWallet,
    types::{Address, Block, Transaction, TransactionRequest, TxHash, H256, U256},
    utils::parse_ether,
};
use eyre::{bail, eyre, Result};
use futures::future::try_join_all;
use log::LevelFilter;
use rand::{distributions::Alphanumeric, Rng};
use sqlx::{Connection, Executor, MySqlConnection, MySqlPool};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    hash::Hash,
    mem,
    time::{Duration, Instant},
};
use tempfile::{tempdir, TempDir};
use tokio::time::{sleep, timeout};

use blob_share::{
    anchor_block::{anchor_block_from_starting_block, persist_anchor_block_to_db},
    client::{DataIntentId, EthProvider, GasPreference, NoncePreference, PostDataResponse},
    consumer::BlobConsumer,
    App, Args, BlockGasSummary, Client, PushMetricsFormat,
};

use crate::{
    geth_helpers::{GethMode, WalletWithProvider},
    run_lodestar::{spawn_lodestar, LodestarInstance, RunLodestarArgs},
    spawn_geth, GethInstance,
};

/// TODO: Shorter finalize depth to trigger functionality faster
pub const FINALIZE_DEPTH: u64 = 8;
pub const ONE_HUNDRED_MS: Duration = Duration::from_millis(100);

#[allow(dead_code)]
pub struct TestHarness {
    pub client: Client,
    base_url: String,
    pub eth_provider: Provider<Http>,
    geth_instance: GethInstance,
    lodestar_instance: Option<LodestarInstance>,
    pub temp_data_dir: TempDir,
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

#[derive(Default)]
pub struct Config {
    initial_topups: HashMap<Address, i128>,
    initial_excess_blob_gas: u128,
}

impl Config {
    pub fn add_initial_topup(mut self, address: Address, balance: i128) -> Self {
        self.initial_topups.insert(address, balance);
        self
    }

    pub fn set_initial_excess_blob_gas(mut self, value: u128) -> Self {
        self.initial_excess_blob_gas = value;
        self
    }
}

impl TestHarness {
    pub async fn spawn_with_el_only() -> Self {
        TestHarness::build(TestMode::ELOnly, None)
            .await
            .spawn_app_in_background()
    }

    #[allow(dead_code)]
    pub async fn spawn_with_chain() -> Self {
        TestHarness::build(TestMode::WithChain, None)
            .await
            .spawn_app_in_background()
    }

    pub async fn build(test_mode: TestMode, test_config: Option<Config>) -> Self {
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

        let eth_provider = Provider::<Http>::try_from(geth_instance.http_url()).unwrap();
        let eth_provider = eth_provider.interval(Duration::from_millis(50));

        // Randomise configuration to ensure test isolation
        let database_name = random_alphabetic_string(16);
        let database_url_without_db = "mysql://root:password@localhost:3306";
        // Create and migrate the database
        configure_database(&database_url_without_db, &database_name).await;
        let database_url = format!("{database_url_without_db}/{database_name}");

        // Apply test config to anchor block
        if let Some(test_config) = test_config {
            let provider = EthProvider::Http(eth_provider.clone());
            let mut anchor_block = anchor_block_from_starting_block(&provider, 0)
                .await
                .unwrap();
            anchor_block.finalized_balances = test_config.initial_topups;
            anchor_block.gas = block_gas_summary_from_excess(test_config.initial_excess_blob_gas);

            let db_pool = connect_db_pool(&database_url).await;
            persist_anchor_block_to_db(&db_pool, anchor_block)
                .await
                .unwrap();
        }

        let temp_data_dir = tempdir().unwrap();

        let args = Args {
            port: 0,
            bind_address: "127.0.0.1".to_string(),
            eth_provider: geth_instance.ws_url().to_string(),
            // Set polling interval to 1 milisecond since anvil auto-mines on each transaction
            eth_provider_interval: Some(1),
            starting_block: 0,
            data_dir: temp_data_dir.path().to_str().unwrap().to_string(),
            mnemonic: Some(
                "work man father plunge mystery proud hollow address reunion sauce theory bonus"
                    .to_string(),
            ),
            panic_on_background_task_errors: true,
            finalize_depth: FINALIZE_DEPTH,
            max_pending_transactions: 6,
            // TODO: De-duplicate of configure properly
            database_url: format!("{database_url_without_db}/{database_name}"),
            metrics: false,
            metrics_port: 0,
            metrics_bearer_token: None,
            metrics_push_url: None,
            metrics_push_interval_sec: 15,
            metrics_push_basic_auth: None,
            metrics_push_format: PushMetricsFormat::PlainText,
        };

        let app = App::build(args).await.unwrap();

        let base_url = format!("http://127.0.0.1:{}", app.port());
        let sender_address = app.sender_address();

        let client = Client::new(&base_url).unwrap();

        Self {
            client,
            base_url,
            eth_provider,
            geth_instance,
            lodestar_instance,
            temp_data_dir,
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
        Fut: Future<Output = ()>,
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
                return Ok(result);
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

    /// Post data with default preferences for random data, may return errors
    pub async fn post_data_of_len(
        &self,
        wallet: &LocalWallet,
        data_len: usize,
    ) -> Result<PostDataResponse> {
        self.client
            .post_data_with_wallet(
                wallet,
                vec![0xff; data_len],
                &GasPreference::RelativeToHead(EthProvider::Http(self.eth_provider.clone()), 1.0),
                &NoncePreference::Timebased,
            )
            .await
    }

    /// Post data with default preferences, expecting no errors
    pub async fn post_data(
        &self,
        wallet: &LocalWallet,
        data: Vec<u8>,
        nonce: Option<NoncePreference>,
    ) -> DataIntentId {
        let res = self
            .client
            .post_data_with_wallet(
                wallet,
                data,
                &GasPreference::RelativeToHead(EthProvider::Http(self.eth_provider.clone()), 1.0),
                &nonce.unwrap_or(NoncePreference::Timebased),
            )
            .await
            .unwrap();

        res.id
    }

    pub async fn post_data_and_wait_for_pending(
        &self,
        wallet: &LocalWallet,
        data: Vec<u8>,
    ) -> DataIntentId {
        let intent_id = self.post_data(wallet, data, None).await;
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
                let known_ids = data_intents
                    .iter()
                    .map(|d| d.id.clone())
                    .collect::<Vec<_>>();
                bail!(
                    "timeout {timeout:?}: waiting for intents {ids:?} not known: {e:?}\nintents known: {known_ids:?}",
                );
            }
        }
    }

    pub async fn wait_for_app_health(&self) {
        retry_with_timeout(
            || async { self.client.health().await },
            Duration::from_secs(1),
            Duration::from_millis(10),
        )
        .await
        .expect("timeout waiting for health")
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

    pub async fn fund_sender_account(&self, wallet: &WalletWithProvider, value: Option<u128>) {
        let sender = self.client.get_sender().await.unwrap();

        let value: U256 = value
            .map(|value| value.into())
            .unwrap_or(parse_ether("0.1").unwrap());

        let tx = TransactionRequest::new()
            .from(wallet.address())
            .to(sender.address)
            .value(value);
        let tx = wallet.send_transaction(tx, None).await.unwrap();
        timeout(Duration::from_secs(30), tx.confirmations(1))
            .await
            .unwrap()
            .unwrap();
    }

    pub fn get_signer_genesis_funds(&self) -> WalletWithProvider {
        self.geth_instance.http_provider().unwrap()
    }
}

async fn connect_db_pool(database_url: &str) -> MySqlPool {
    MySqlPool::connect(&database_url)
        .await
        .expect(&format!("Failed to connect to DB {database_url}"))
}

async fn configure_database(database_url_without_db: &str, database_name: &str) -> MySqlPool {
    println!("connecting to MySQL {database_url_without_db}, creating ephemeral database_name '{database_name}'");

    // Create database
    let mut connection = MySqlConnection::connect(database_url_without_db)
        .await
        .expect("Failed to connect to DB");
    connection
        .execute(&*format!("CREATE DATABASE {};", database_name))
        .await
        .expect("Failed to create database.");

    // Migrate database
    let database_url = format!("{database_url_without_db}/{database_name}");
    let connection_pool = connect_db_pool(&database_url).await;
    sqlx::migrate!("./migrations")
        .run(&connection_pool)
        .await
        .expect("Failed to migrate the database");

    connection_pool
}

/// Test mock BlockGasSummary from `excess_blob_gas` only
fn block_gas_summary_from_excess(excess_blob_gas: u128) -> BlockGasSummary {
    let mut block = Block::<TxHash>::default();
    block.excess_blob_gas = Some(excess_blob_gas.into());
    block.blob_gas_used = Some(0.into());
    block.base_fee_per_gas = Some(0.into());
    BlockGasSummary::from_block(&block).unwrap()
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

/// Returns unique elements of slice `v`
pub fn unique<T: Eq + Hash>(v: &[T]) -> Vec<&T> {
    v.iter().collect::<HashSet<&T>>().into_iter().collect()
}

fn random_alphabetic_string(length: usize) -> String {
    let mut rng = rand::thread_rng();
    std::iter::repeat(())
        .map(|()| rng.sample(Alphanumeric))
        .filter(|c| c.is_ascii_alphabetic())
        .take(length)
        .map(char::from)
        .collect()
}
