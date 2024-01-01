use actix_web::dev::Server;
use actix_web::{web, App, HttpResponse, HttpServer};
use blob_share::utils::tx_reth_to_ethers;
use blob_share::{
    compute_blob_tx_hash,
    utils::{deserialize_blob_tx_pooled, hex_0x_prefix_to_vec, vec_to_hex_0x_prefix},
};
use ethers::providers::{Http, Provider};
use ethers::types::{Address, Block, Transaction, H256};
use eyre::{bail, eyre, Result};
use log::{error, info};
use rand::{thread_rng, Rng};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::mem;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

// Define your JSON RPC request and response structures as before

// Mock Ethereum Server Struct
pub struct MockEthereumServer {
    server: ServerStatus,
    port: u16,
    data: Arc<Mutex<ServerData>>,
}

enum ServerStatus {
    Built(Server),
    Running,
}

type Hash = [u8; 32];

#[derive(Default)]
struct ServerData {
    head_number: u64,
    blocks_by_hash: HashMap<String, Block<Transaction>>,
    blocks_by_number: HashMap<u64, Block<Transaction>>,
    nonce_per_address: HashMap<Address, u64>,
    tx_pool: HashMap<Hash, Transaction>,
    next_filter_id: usize,
    block_filters: HashMap<usize, Vec<H256>>,
    chain_id: u64,
}

impl MockEthereumServer {
    // Function to start the server
    pub async fn build() -> Self {
        // Find a random available port
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let data = Arc::new(Mutex::new(ServerData {
            head_number: 0,
            blocks_by_hash: <_>::default(),
            blocks_by_number: <_>::default(),
            nonce_per_address: <_>::default(),
            tx_pool: <_>::default(),
            next_filter_id: 0,
            block_filters: <_>::default(),
            chain_id: 69420,
        }));
        let data_clone = data.clone();

        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(data_clone.clone()))
                .route("/", web::post().to(post_root))
        })
        .listen(listener)
        .unwrap()
        .run();

        MockEthereumServer {
            server: ServerStatus::Built(server),
            port,
            data,
        }
    }

    pub fn spawn_app_in_background(mut self) -> Self {
        match mem::replace(&mut self.server, ServerStatus::Running) {
            ServerStatus::Running => panic!("already running"),
            ServerStatus::Built(app) => {
                // Run app server in the background
                let _ = tokio::spawn(app);
            }
        }
        self
    }

    pub fn with_genesis_block(self) -> Self {
        let block = get_block_with_txs(0, H256([0xab; 32]));
        self.add_block(block);
        self
    }

    pub fn http_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn get_ethers_provider(&self) -> Provider<Http> {
        Provider::<Http>::try_from(self.http_url()).unwrap()
    }

    pub fn get_chain_id(&self) -> u64 {
        self.data.lock().unwrap().chain_id
    }

    pub fn add_block(&self, block: Block<Transaction>) {
        let block_hash = block.hash.unwrap();
        let block_number = block.number.unwrap();
        info!(
            "added block {block_number} {block_hash}, with {} txs",
            block.transactions.len()
        );

        let mut data = self.data.lock().unwrap();
        data.blocks_by_hash
            .insert(tx_hash_to_hex(block_hash), block.clone());
        data.blocks_by_number.insert(block_number.as_u64(), block);

        for filter in data.block_filters.values_mut() {
            filter.push(block_hash)
        }
    }

    pub fn mine_block_with<F: FnOnce(&mut Block<Transaction>)>(
        &self,
        f_mut_block: F,
    ) -> Block<Transaction> {
        // Drop data lock before calling `self.add_block()`
        let head = {
            let data = self.data.lock().unwrap();
            data.blocks_by_number
                .get(&data.head_number)
                .expect("no block for head_number")
                .clone()
        };

        let head_number = head.number.unwrap().as_u64();
        info!("mined block on head {} {:?}", head_number, head.hash);

        let new_head_number = head_number + 1;
        let mut block = get_block_with_txs(new_head_number, generate_random_hash());

        block.parent_hash = head.hash.unwrap();
        f_mut_block(&mut block);

        self.add_block(block.clone());

        // Set new block as head
        self.data.lock().unwrap().head_number = new_head_number;

        block
    }

    pub fn get_submitted_tx(&self, tx_hash: H256) -> Transaction {
        let data = self.data.lock().unwrap();
        data.tx_pool
            .get(&tx_hash.to_fixed_bytes())
            .expect(&format!(
                "no transaction knonw for hash {}",
                hex::encode(tx_hash)
            ))
            .clone()
    }

    pub fn get_block_subscription_count(&self) -> usize {
        self.data.lock().unwrap().block_filters.len()
    }
}

fn tx_hash_to_hex(tx_hash: H256) -> String {
    format!("0x{}", hex::encode(tx_hash.to_fixed_bytes()))
}

fn generate_random_hash() -> H256 {
    let mut rng = thread_rng();
    let mut arr = [0u8; 32];
    rng.fill(&mut arr);
    H256(arr)
}

// Define a structure for the JSON RPC request
#[derive(Deserialize)]
struct JsonRpcRequest {
    method: String,
    params: Option<serde_json::Value>,
    id: u64,
}

impl JsonRpcRequest {
    fn get_param<T: DeserializeOwned>(&self, i: usize) -> Result<T> {
        let param_value = self
            .params
            .as_ref()
            .ok_or_else(|| eyre!("no params field"))?
            .get(i)
            .ok_or_else(|| eyre!("missing param[{i}]"))?
            .clone();
        Ok(serde_json::from_value(param_value)?)
    }

    fn get_param_hex_u64(&self, i: usize) -> Result<u64> {
        let hex_str: String = self.get_param(i)?;
        let hex_str = hex_str.trim_start_matches("0x");
        Ok(u64::from_str_radix(hex_str, 16)?)
    }
}

fn value_to_hex(v: u64) -> String {
    format!("0x{:x}", v)
}

// Define a structure for the JSON RPC response
#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    result: serde_json::Value,
    id: u64,
}

fn handle_ethereum_rpc(data: &mut ServerData, req: &JsonRpcRequest) -> Result<serde_json::Value> {
    Ok(match req.method.as_str() {
        "eth_chainId" => serde_json::to_value(value_to_hex(data.chain_id))?,

        "eth_blockNumber" => serde_json::to_value(value_to_hex(data.head_number))?,

        "eth_getBlockByHash" => {
            let hash: String = req.get_param(0)?;
            let block = data
                .blocks_by_hash
                .get(&hash)
                .ok_or_else(|| eyre!(format!("unknown block {hash}")))?;
            serde_json::to_value(block)?
        }

        "eth_getBlockByNumber" => {
            if let Ok(block_tag) = req.get_param::<String>(0) {
                if block_tag == "latest" {
                    let block = data
                        .blocks_by_number
                        .get(&data.head_number)
                        .ok_or_else(|| eyre!(format!("no head block")))?;
                    return Ok(serde_json::to_value(block)?);
                }
            }
            let number = req.get_param_hex_u64(0)?;
            let block = data
                .blocks_by_number
                .get(&number)
                .ok_or_else(|| eyre!(format!("unknown block {number}")))?;
            serde_json::to_value(block)?
        }

        "eth_sendRawTransaction" => {
            let blob_tx_rlp = req.get_param::<String>(0)?;
            let blob_tx_rlp = hex_0x_prefix_to_vec(&blob_tx_rlp)?;
            let blob_tx = deserialize_blob_tx_pooled(&blob_tx_rlp)?;
            let (tx_hash, _) = compute_blob_tx_hash(&blob_tx.transaction, &blob_tx.signature);

            // TODO: Silly conversions..
            let tx = tx_reth_to_ethers(&blob_tx.transaction).unwrap();
            data.tx_pool.insert(tx_hash, tx);

            serde_json::to_value(vec_to_hex_0x_prefix(&tx_hash))?
        }

        "eth_getTransactionCount" => {
            let address: Address = req.get_param(0)?;
            let nonce = data.nonce_per_address.get(&address).copied().unwrap_or(0);
            serde_json::to_value(value_to_hex(nonce))?
        }

        "eth_newBlockFilter" => {
            let id = data.next_filter_id;
            data.next_filter_id = id + 1;
            data.block_filters.insert(id, vec![]);
            serde_json::to_value(value_to_hex(id as u64))?
        }

        "eth_getFilterChanges" => {
            let id = req.get_param_hex_u64(0)? as usize;
            let filter = data
                .block_filters
                .get_mut(&id)
                .ok_or_else(|| eyre!("unknown filter id {id}"))?;
            // get the new hashes and clear the array
            let new_hashes = filter.drain(..).collect::<Vec<_>>();
            serde_json::to_value(new_hashes)?
        }

        // Returns a valid but empty response, can cause estimators to return bad results
        // Ref: https://docs.alchemy.com/reference/eth-feehistory
        "eth_feeHistory" => serde_json::to_value(FeeHistory {
            oldestBlock: "0x0".to_string(),
            baseFeePerGas: vec![],
            gasUsedRatio: vec![],
        })?,

        _ => bail!("unknown route {}", req.method.as_str()),
    })
}

#[allow(non_snake_case)]
#[derive(Serialize)]
struct FeeHistory {
    oldestBlock: String,
    baseFeePerGas: Vec<String>,
    gasUsedRatio: Vec<f64>,
}

async fn post_root(
    data: web::Data<Arc<Mutex<ServerData>>>,
    req: web::Json<JsonRpcRequest>,
) -> Result<HttpResponse, actix_web::Error> {
    Ok(HttpResponse::Ok().json(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: handle_ethereum_rpc(&mut data.lock().unwrap(), &req).map_err(|e| {
            error!("error handling JSON RPC response: {:?}", e);
            actix_web::error::ErrorInternalServerError(e)
        })?,
        id: req.id,
    }))
}

pub fn get_block_with_txs(number: u64, hash: H256) -> Block<Transaction> {
    let mut block = Block::<Transaction>::default();
    block.number = Some(number.into());
    block.hash = Some(hash);
    block.base_fee_per_gas = Some(0.into());
    block.blob_gas_used = Some(0.into());
    block.excess_blob_gas = Some(0.into());
    block
}

#[cfg(test)]
mod tests {
    use ethers::providers::Middleware;

    use super::*;

    #[tokio::test]
    async fn retrieve_block_from_mock() {
        let mock_server = MockEthereumServer::build().await.spawn_app_in_background();
        let provider = mock_server.get_ethers_provider();

        let number = 123;
        let hash = H256([0xaa; 32]);
        let block = get_block_with_txs(number, hash);
        mock_server.add_block(block);

        provider.get_block_with_txs(number).await.unwrap();
        provider.get_block_with_txs(hash).await.unwrap();
    }
}
