use ethers::{
    middleware::SignerMiddleware,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::{Address, Block, Transaction, TransactionRequest, TxHash, H256, U256},
    utils::parse_ether,
};
use eyre::{bail, eyre, Result};
use futures::future::try_join_all;
use lazy_static::lazy_static;
use log::{info, LevelFilter};
use rand::{distributions::Alphanumeric, Rng};
use sqlx::{Connection, Executor, MySqlConnection, MySqlPool};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    hash::Hash,
    mem,
    str::FromStr,
    time::{Duration, Instant},
};
use tempfile::{tempdir, TempDir};
use tokio::time::{sleep, timeout};

use blob_share::{
    anchor_block::{anchor_block_from_starting_block, persist_anchor_block_to_db},
    client::{DataIntentId, EthProvider, GasPreference, NoncePreference, PostDataResponse},
    consumer::BlobConsumer,
    get_blob_gasprice, App, Args, BlockGasSummary, Client, PushMetricsFormat,
};

use crate::{
    geth_helpers::GethMode,
    mock_el_server::MockEthereumServer,
    run_lodestar::{spawn_lodestar, LodestarInstance, RunLodestarArgs},
    spawn_geth, GethInstance,
};

/// TODO: Shorter finalize depth to trigger functionality faster
pub const FINALIZE_DEPTH: u64 = 8;
pub const ONE_HUNDRED_MS: Duration = Duration::from_millis(100);
pub const GWEI_TO_WEI: u64 = 1_000_000_000;
pub const ETH_TO_WEI: u64 = 1_000_000_000 * GWEI_TO_WEI;

const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";
const DEV_PUBKEY: &str = "0xdbD48e742FF3Ecd3Cb2D557956f541b6669b3277";

lazy_static! {
    pub static ref GENESIS_FUNDS_ADDR: Address = Address::from_str(DEV_PUBKEY).unwrap();
}

#[allow(dead_code)]
pub struct TestHarness {
    pub client: Client,
    base_url: String,
    mock_ethereum_server: Option<MockEthereumServer>,
    geth_instance: Option<GethInstance>,
    lodestar_instance: Option<LodestarInstance>,
    pub temp_data_dir: TempDir,
    pub sender_address: Address,
    app_status: AppStatus,
}

pub enum TestMode {
    ELMock,
    ELOnly,
    ELAndCL,
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
        TestHarness::build(TestMode::ELAndCL, None)
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

        let (geth_instance, lodestar_instance, mock_ethereum_server) = match test_mode {
            TestMode::ELMock => (
                None,
                None,
                Some(
                    MockEthereumServer::build()
                        .await
                        .spawn_app_in_background()
                        .with_genesis_block(),
                ),
            ),
            TestMode::ELOnly => {
                let geth = spawn_geth(GethMode::Dev).await;
                (Some(geth), None, None)
            }
            TestMode::ELAndCL => {
                let geth = spawn_geth(GethMode::Interop).await;
                let lodestar = spawn_lodestar(RunLodestarArgs {
                    execution_url: geth.authrpc_url(),
                    genesis_eth1_hash: geth.genesis_block_hash_hex(),
                })
                .await;
                (Some(geth), Some(lodestar), None)
            }
        };

        let eth_provider_urls = get_eth_provider_urls(&geth_instance, &mock_ethereum_server);

        // Randomise configuration to ensure test isolation
        let database_name = random_alphabetic_string(16);
        let database_url_without_db = "mysql://root:password@localhost:3306";
        // Create and migrate the database
        configure_database(&database_url_without_db, &database_name).await;
        let database_url = format!("{database_url_without_db}/{database_name}");

        // Apply test config to anchor block
        if let Some(test_config) = test_config {
            let provider = Provider::<Http>::try_from(&eth_provider_urls.http).unwrap();
            let provider = EthProvider::Http(provider.clone());
            let mut anchor_block = anchor_block_from_starting_block(&provider, 0)
                .await
                .unwrap();
            anchor_block.finalized_balances = test_config.initial_topups;
            anchor_block.gas = block_gas_summary_from_excess(
                test_config.initial_excess_blob_gas,
                &anchor_block.gas,
            );

            let db_pool = connect_db_pool(&database_url).await;
            persist_anchor_block_to_db(&db_pool, anchor_block)
                .await
                .unwrap();
        }

        let temp_data_dir = tempdir().unwrap();

        let args = Args {
            port: 0,
            bind_address: "127.0.0.1".to_string(),
            // Use websockets when using actual Geth to test that path
            eth_provider: eth_provider_urls.ws.unwrap_or(eth_provider_urls.http),
            // Set polling interval to 10 milisecond since anvil auto-mines on each transaction
            eth_provider_interval: Some(10),
            starting_block: 0,
            data_dir: temp_data_dir.path().to_str().unwrap().to_string(),
            mnemonic: Some(
                "work man father plunge mystery proud hollow address reunion sauce theory bonus"
                    .to_string(),
            ),
            panic_on_background_task_errors: true,
            finalize_depth: FINALIZE_DEPTH,
            max_pending_transactions: 6,
            fee_estimator: blob_share::FeeEstimator::Default,
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
            mock_ethereum_server,
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

    pub async fn spawn_with_fn<F, Fut>(mut self, f: F)
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
                result.expect("app running in background error");
                return;
            }
            _ = f_future => {
                // This branch is executed if f() finishes first
                // f() can finish by completing the test successfully, or by encountering some error
                return;
            }
        }
    }

    pub fn eth_provider_http(&self) -> Provider<Http> {
        Provider::<Http>::try_from(self.eth_provider_urls().http)
            .unwrap()
            .interval(Duration::from_millis(50))
    }

    pub fn geth(&self) -> &GethInstance {
        self.geth_instance
            .as_ref()
            .expect("not using geth instance")
    }

    pub fn mock_el(&self) -> &MockEthereumServer {
        self.mock_ethereum_server
            .as_ref()
            .expect("not using mock EL")
    }

    fn eth_provider_urls(&self) -> EthProviderURLs {
        get_eth_provider_urls(&self.geth_instance, &self.mock_ethereum_server)
    }

    pub fn get_blob_consumer(&self, participant_address: Address) -> BlobConsumer {
        let beacon_api_base_url = match &self.lodestar_instance {
            Some(lodestar_instance) => lodestar_instance.http_url(),
            None => panic!("no beacon node is configured"),
        };

        BlobConsumer::new(
            beacon_api_base_url,
            &self.eth_provider_urls().http,
            self.sender_address,
            participant_address,
        )
        .unwrap()
    }

    /// Post data with default preferences for random data, may return errors
    pub async fn post_data(
        &self,
        wallet: &LocalWallet,
        data_req: DataReq,
    ) -> Result<PostDataResponse> {
        let data = if let Some(data) = data_req.data {
            data
        } else {
            let data_len = data_req.data_len.unwrap_or(1000);
            vec![0xff; data_len]
        };

        let gas_preference = if let Some(max_blob_gas) = data_req.max_blob_gas {
            GasPreference::Value(max_blob_gas)
        } else {
            GasPreference::RelativeToHead(EthProvider::Http(self.eth_provider_http()), 1.0)
        };

        let nonce_preference = if let Some(nonce) = data_req.nonce {
            NoncePreference::Value(nonce)
        } else {
            NoncePreference::Timebased
        };

        info!(
            "posting data of len {} {:?} {:?}",
            data.len(),
            gas_preference,
            nonce_preference
        );

        self.client
            .post_data_with_wallet(wallet, data, &gas_preference, &nonce_preference)
            .await
    }

    /// Post data with default preferences, expecting no errors
    pub async fn post_data_ok(&self, wallet: &LocalWallet, data_req: DataReq) -> DataIntentId {
        self.post_data(wallet, data_req).await.unwrap().id
    }

    pub async fn post_data_and_wait_for_pending(
        &self,
        wallet: &LocalWallet,
        data_req: DataReq,
    ) -> DataIntentId {
        let intent_id = self.post_data_ok(wallet, data_req).await;
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
        let eth_provider = self.eth_provider_http();

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
                let sender_nonce = eth_provider
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
    ) -> Vec<H256> {
        self.wait_for_intent_inclusion_in_any_tx_with_filter(ids, timeout, &None)
            .await
    }

    pub async fn wait_for_intent_inclusion_in_any_tx_with_filter(
        &self,
        ids: &[DataIntentId],
        timeout: Duration,
        exclude_transaction_ids: &Option<Vec<H256>>,
    ) -> Vec<H256> {
        match retry_with_timeout(
            || async {
                let mut tx_hashes = vec![];
                for id in ids {
                    let tx_hash = self
                        .client
                        .get_status_by_id(*id)
                        .await?
                        .is_in_tx()
                        .ok_or_else(|| eyre!("still pending"))?;
                    if let Some(exclude_transaction_ids) = exclude_transaction_ids {
                        if exclude_transaction_ids.contains(&tx_hash) {
                            bail!("still included in an excluded transaction {tx_hash}")
                        }
                    }
                    tx_hashes.push(tx_hash);
                }
                Ok(tx_hashes)
            },
            timeout,
            ONE_HUNDRED_MS,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                let statuses = try_join_all(
                    ids.iter()
                        .map(|id| self.client.get_status_by_id(*id))
                        .collect::<Vec<_>>(),
                )
                .await
                .unwrap();
                panic!(
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
        let eth_provider = self.eth_provider_http();
        let mut txs = vec![];
        let head_number = eth_provider.get_block_number().await?.as_u64();
        for block_number in (1..=head_number).rev() {
            let block = eth_provider
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
        get_signer_genesis_funds(&self.eth_provider_urls().http, self.chain_id()).unwrap()
    }

    pub fn chain_id(&self) -> u64 {
        if let Some(mock_el) = &self.mock_ethereum_server {
            mock_el.get_chain_id()
        } else if let Some(geth) = &self.geth_instance {
            geth.get_chain_id()
        } else {
            unreachable!("no EL")
        }
    }

    pub async fn mine_block_and_wait_for_sync<F: FnOnce(&mut Block<Transaction>)>(
        &self,
        f_mut_block: F,
    ) {
        // Ensure app is subscribed to blocks before mining first block
        self.wait_for_block_subscription().await;

        let block = self.mock_el().mine_block_with(f_mut_block);
        let block_number = block.number.expect("block has no hash");
        let block_hash = block.hash.expect("block has no hash");

        retry_with_timeout(
            || async {
                let sync = self.client.get_sync().await.unwrap();
                if sync.synced_head.hash == block_hash {
                    Ok(())
                } else {
                    bail!(
                        "synced head {:?} expected {block_number} {block_hash}",
                        sync.synced_head
                    )
                }
            },
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .await
        .expect(&format!(
            "timeout waiting to sync mined block {block_number} {block_hash}"
        ))
    }

    /// Use to resolve race condition where the test mines a block before the app runner has
    /// subscribed to blocks, missing the event of the newly mined block
    pub async fn wait_for_block_subscription(&self) {
        retry_with_timeout(
            || async {
                if self.mock_el().get_block_subscription_count() > 0 {
                    Ok(())
                } else {
                    bail!("no block subscriptions")
                }
            },
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .await
        .expect("timeout waiting for app to subscribe to mock EL block filter");
    }
}

struct EthProviderURLs {
    http: String,
    ws: Option<String>,
}

fn get_eth_provider_urls(
    geth_instance: &Option<GethInstance>,
    mock_el: &Option<MockEthereumServer>,
) -> EthProviderURLs {
    if let Some(mock_el) = &mock_el {
        EthProviderURLs {
            http: mock_el.http_url(),
            ws: None,
        }
    } else if let Some(geth_instance) = &geth_instance {
        EthProviderURLs {
            http: geth_instance.http_url().to_string(),
            ws: Some(geth_instance.ws_url().to_string()),
        }
    } else {
        unreachable!("no EL")
    }
}

#[derive(Default)]
pub struct DataReq {
    data: Option<Vec<u8>>,
    data_len: Option<usize>,
    max_blob_gas: Option<u64>,
    nonce: Option<u64>,
}

impl DataReq {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_data(mut self, data: Vec<u8>) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_data_len(mut self, data_len: usize) -> Self {
        self.data_len = Some(data_len);
        self
    }

    pub fn with_max_blob_gas(mut self, max_blob_gas: u64) -> Self {
        self.max_blob_gas = Some(max_blob_gas);
        self
    }

    pub fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = Some(nonce);
        self
    }
}

pub type WalletWithProvider = SignerMiddleware<Provider<Http>, LocalWallet>;

pub fn get_wallet_genesis_funds() -> LocalWallet {
    LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY).unwrap()).unwrap()
}

pub fn get_signer_genesis_funds(
    eth_provider_url: &str,
    chain_id: u64,
) -> Result<WalletWithProvider> {
    let wallet = get_wallet_genesis_funds();
    assert_eq!(wallet.address(), *GENESIS_FUNDS_ADDR);
    let provider = Provider::<Http>::try_from(eth_provider_url)?;

    Ok(SignerMiddleware::new(
        provider,
        wallet.with_chain_id(chain_id),
    ))
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
fn block_gas_summary_from_excess(
    excess_blob_gas: u128,
    prev_gas_summary: &BlockGasSummary,
) -> BlockGasSummary {
    let mut block = Block::<TxHash>::default();
    block.excess_blob_gas = Some(excess_blob_gas.into());
    block.blob_gas_used = Some(0.into());
    block.base_fee_per_gas = Some(prev_gas_summary.base_fee_per_gas.into());
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

const MIN_BLOB_GASPRICE: u128 = 1;
const BLOB_GASPRICE_UPDATE_FRACTION: u128 = 3338477;

pub fn find_excess_blob_gas(blob_gas_price: u128) -> u128 {
    let denominator = BLOB_GASPRICE_UPDATE_FRACTION;
    let factor = MIN_BLOB_GASPRICE;
    let result = blob_gas_price;

    // numerator = (ln(result) - ln(factor)) * denominator
    (((result as f64).ln() - (factor as f64).ln()) * denominator as f64) as u128
}

pub fn assert_close_enough(a: u64, b: u64, epsilon: u64) {
    let max = a.max(b);
    let min = a.min(b);

    if max - min > epsilon {
        panic!(
            "Assertion failed: {} and {} are not close enough (epsilon: {})",
            a, b, epsilon
        );
    }
}

#[test]
fn test_find_excess_blob_gas() {
    assert_eq!(find_excess_blob_gas(1_000_000_000), 69184146);
    assert_eq!(get_blob_gasprice(69184146), 999999890);
    assert_eq!(get_blob_gasprice(69184147), 1000000190);
}
