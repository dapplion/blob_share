use actix_web::{get, web, HttpResponse, Responder};
use ethers::signers::Signer;
use ethers::types::{Address, TxHash, H256};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod post_data;

use crate::data_intent::DataIntentId;
use crate::data_intent_tracker::{
    fetch_data_intent_db_full, DataIntentDbRowFull, DataIntentSummary,
};
use crate::eth_provider::EthProvider;
use crate::sync::AnchorBlock;
use crate::utils::e500;
use crate::AppData;
pub use post_data::{PostDataIntentV1, PostDataIntentV1Signed, PostDataResponse};

// TODO: Add route to cancel data intents by ID

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
    let (anchor_block, synced_head) = data.get_sync().await;
    Ok(HttpResponse::Ok().json(SyncStatus {
        anchor_block,
        synced_head,
        node_head: get_node_head(&data.provider).await.map_err(e500)?,
    }))
}

// #[post("/v1/data")}
// post_data
// > MOVED to routes/post_data.rs

#[get("/v1/data")]
pub(crate) async fn get_data(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let items: Vec<DataIntentSummary> = data.get_all_pending().await;
    Ok(HttpResponse::Ok().json(items))
}

#[get("/v1/data/{id}")]
pub(crate) async fn get_data_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<DataIntentId>,
) -> Result<HttpResponse, actix_web::Error> {
    // TODO: Try to unify types, too many `DataIntent*` things
    let item: DataIntentDbRowFull = fetch_data_intent_db_full(&data.db_pool, &id)
        .await
        .map_err(e500)?;
    Ok(HttpResponse::Ok().json(item))
}

#[get("/v1/status/{id}")]
pub(crate) async fn get_status_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<DataIntentId>,
) -> Result<HttpResponse, actix_web::Error> {
    let status: DataIntentStatus = data.status_by_id(&id).await.map_err(e500)?;
    Ok(HttpResponse::Ok().json(status))
}

#[tracing::instrument(skip(data))]
#[get("/v1/balance/{address}")]
pub(crate) async fn get_balance_by_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> Result<HttpResponse, actix_web::Error> {
    let balance: i128 = data.balance_of_user(&address).await;
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

impl From<&AnchorBlock> for SyncStatusBlock {
    fn from(val: &AnchorBlock) -> Self {
        Self {
            number: val.number,
            hash: val.hash,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncStatus {
    pub anchor_block: SyncStatusBlock,
    pub synced_head: SyncStatusBlock,
    pub node_head: SyncStatusBlock,
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
