use actix_web::{get, post, web, HttpResponse, Responder};
use ethers::signers::Signer;
use ethers::types::{Address, TxHash, H256};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use std::str::FromStr;
use std::sync::Arc;

use crate::data_intent::{deserialize_signature, DataHash, DataIntent, DataIntentId};
use crate::data_intent_tracker::DataIntentItemStatus;
use crate::eth_provider::EthProvider;
use crate::sync::TxInclusion;
use crate::utils::{e400, e500};
use crate::AppData;

#[get("/")]
pub(crate) async fn get_home() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body("<html><body><h1>Blob share API</h1></body></html>")
}

#[get("/v1/health")]
pub(crate) async fn get_health() -> impl Responder {
    HttpResponse::Ok().finish()
}

#[get("/v1/sender")]
pub(crate) async fn get_sender(data: web::Data<Arc<AppData>>) -> impl Responder {
    HttpResponse::Ok().json(SenderDetails {
        address: data.sender_wallet.address(),
    })
}

#[get("/v1/sync")]
pub(crate) async fn get_sync(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    Ok(HttpResponse::Ok().json(SyncStatus {
        anchor_block: data.sync.read().await.get_anchor(),
        synced_head: data.sync.read().await.get_head(),
        node_head: get_node_head(&data.provider).await.map_err(e500)?,
    }))
}

#[post("/v1/data")]
pub(crate) async fn post_data(
    body: web::Json<PostDataIntentV1>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let data_intent: DataIntent = body.into_inner().try_into().map_err(e400)?;
    data_intent.verify_signature().map_err(e400)?;

    let account_balance = data
        .sync
        .read()
        .await
        .balance_with_pending(data_intent.from);
    if account_balance < data_intent.max_cost() as i128 {
        return Err(e400(eyre!(
            "Insufficient balance, current balance {} requested {}",
            account_balance,
            data_intent.max_cost()
        )));
    }

    let id = data
        .data_intent_tracker
        .write()
        .await
        .add(data_intent)
        .map_err(e500)?;

    data.notify.notify_one();

    Ok(HttpResponse::Ok().json(PostDataResponse { id: id.to_string() }))
}

#[get("/v1/data")]
pub(crate) async fn get_data(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let items: Vec<DataIntentSummary> = { data.data_intent_tracker.read().await.get_all_pending() }
        .iter()
        .map(|item| item.into())
        .collect();
    Ok(HttpResponse::Ok().json(items))
}

#[get("/v1/data/{id}")]
pub(crate) async fn get_data_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = DataIntentId::from_str(&id).map_err(e400)?;
    let item: DataIntent = {
        data.data_intent_tracker
            .read()
            .await
            .data_by_id(&id)
            .ok_or_else(|| e400(format!("no item found for ID {}", id)))?
    };
    Ok(HttpResponse::Ok().json(item))
}

#[get("/v1/status/{id}")]
pub(crate) async fn get_status_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = DataIntentId::from_str(&id).map_err(e400)?;
    let status = { data.data_intent_tracker.read().await.status_by_id(&id) };

    let status = match status {
        DataIntentItemStatus::Pending => DataIntentStatus::Pending,
        DataIntentItemStatus::Unknown => DataIntentStatus::Unknown,
        DataIntentItemStatus::Included(tx_hash) => {
            match data.sync.read().await.get_tx_status(tx_hash) {
                Some(TxInclusion::Pending) => DataIntentStatus::InPendingTx { tx_hash },
                Some(TxInclusion::Included(block_hash)) => DataIntentStatus::InConfirmedTx {
                    tx_hash,
                    block_hash,
                },
                None => {
                    // Should never happen, review this case
                    DataIntentStatus::Unknown
                }
            }
        }
    };

    Ok(HttpResponse::Ok().json(status))
}

#[get("/v1/balance/{address}")]
pub(crate) async fn get_balance_by_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> Result<HttpResponse, actix_web::Error> {
    let balance = data.sync.read().await.balance_with_pending(*address);
    Ok(HttpResponse::Ok().json(balance))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SenderDetails {
    pub address: Address,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncStatusBlock {
    pub hash: H256,
    pub number: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncStatus {
    pub anchor_block: SyncStatusBlock,
    pub synced_head: SyncStatusBlock,
    pub node_head: SyncStatusBlock,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DataIntentSummary {
    pub id: String,
    pub from: Address,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub data_len: usize,
    pub max_blob_gas_price: u128,
}

impl From<&DataIntent> for DataIntentSummary {
    fn from(value: &DataIntent) -> Self {
        Self {
            id: value.id().to_string(),
            from: value.from,
            data_hash: value.data_hash.to_vec(),
            data_len: value.data.len(),
            max_blob_gas_price: value.max_blob_gas_price,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataResponse {
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1 {
    /// Address sending the data
    pub from: Address,
    /// Data to be posted
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>,
    #[serde(with = "hex_vec")]
    pub signature: Vec<u8>,
    /// Max price user is willing to pay in wei
    pub max_blob_gas_price: u128,
}

impl TryInto<DataIntent> for PostDataIntentV1 {
    type Error = eyre::Report;
    fn try_into(self) -> Result<DataIntent, Self::Error> {
        let data_hash = DataHash::from_data(&self.data);
        Ok(DataIntent {
            from: self.from,
            data: self.data,
            data_hash,
            signature: deserialize_signature(&self.signature)?,
            max_blob_gas_price: self.max_blob_gas_price,
        })
    }
}

impl From<DataIntent> for PostDataIntentV1 {
    fn from(value: DataIntent) -> Self {
        Self {
            from: value.from,
            data: value.data,
            signature: value.signature.to_vec(),
            max_blob_gas_price: value.max_blob_gas_price,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DataIntentStatus {
    Unknown,
    Pending,
    InPendingTx { tx_hash: TxHash },
    InConfirmedTx { tx_hash: TxHash, block_hash: H256 },
}

impl DataIntentStatus {
    pub fn is_known(&self) -> bool {
        match self {
            DataIntentStatus::InConfirmedTx { .. }
            | DataIntentStatus::InPendingTx { .. }
            | DataIntentStatus::Pending => true,
            DataIntentStatus::Unknown => false,
        }
    }

    pub fn is_in_tx(&self) -> Option<TxHash> {
        match self {
            DataIntentStatus::Unknown | DataIntentStatus::Pending => None,
            DataIntentStatus::InPendingTx { tx_hash } => Some(*tx_hash),
            DataIntentStatus::InConfirmedTx { tx_hash, .. } => Some(*tx_hash),
        }
    }

    pub fn is_in_block(&self) -> Option<(H256, TxHash)> {
        match self {
            DataIntentStatus::Unknown
            | DataIntentStatus::Pending
            | DataIntentStatus::InPendingTx { .. } => None,
            DataIntentStatus::InConfirmedTx {
                tx_hash,
                block_hash,
            } => Some((*tx_hash, *block_hash)),
        }
    }
}

/// Fetch execution node head block number and hash
async fn get_node_head(provider: &EthProvider) -> Result<SyncStatusBlock> {
    let node_head_number = provider.get_block_number().await?.as_u64();
    let node_head_block = provider
        .get_block(node_head_number)
        .await?
        .ok_or_else(|| eyre!("no block for number {}", node_head_number))?;
    Ok(SyncStatusBlock {
        number: node_head_number,
        hash: node_head_block
            .hash
            .ok_or_else(|| eyre!("block number {} has not hash", node_head_number))?,
    })
}

#[cfg(test)]
mod tests {

    use ethers::types::Address;
    use eyre::Result;
    use std::str::FromStr;

    use crate::routes::PostDataIntentV1;

    #[test]
    fn route_post_data_intent_v1_serde() -> Result<()> {
        let data_intent = PostDataIntentV1 {
            from: Address::from_str("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")?,
            data: vec![0xaa; 50],
            signature: vec![0xbb; 65],
            max_blob_gas_price: 1000000000,
        };

        assert_eq!(&serde_json::to_string(&data_intent)?, "{\"from\":\"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\",\"data\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"signature\":\"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"max_blob_gas_price\":1000000000}");

        Ok(())
    }
}
