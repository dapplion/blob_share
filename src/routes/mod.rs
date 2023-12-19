use actix_web::{get, web, HttpResponse, Responder};
use ethers::signers::Signer;
use ethers::types::{Address, TxHash, H256};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

pub mod post_data;

use crate::data_intent::{DataIntent, DataIntentId, DataIntentSummary};
use crate::data_intent_tracker::DataIntentItemStatus;
use crate::eth_provider::EthProvider;
use crate::sync::{AnchorBlock, TxInclusion};
use crate::utils::{e400, e500};
use crate::AppData;
pub use post_data::{PostDataIntentV1, PostDataIntentV1Signed, PostDataResponse};

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
        anchor_block: data.sync.read().await.get_anchor().into(),
        synced_head: data.sync.read().await.get_head(),
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
        DataIntentItemStatus::Unknown => DataIntentStatus::Unknown,
        DataIntentItemStatus::Evicted => DataIntentStatus::Evicted,
        DataIntentItemStatus::Pending => DataIntentStatus::Pending,
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

#[tracing::instrument(skip(data))]
#[get("/v1/balance/{address}")]
pub(crate) async fn get_balance_by_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> Result<HttpResponse, actix_web::Error> {
    let balance: i128 = data.balance_of_user(&address).await;
    Ok(HttpResponse::Ok().json(balance))
}

#[tracing::instrument(skip(data))]
#[get("/v1/last_seen_nonce/{address}")]
pub(crate) async fn get_last_seen_nonce_by_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> Result<HttpResponse, actix_web::Error> {
    let nonce: Option<u128> = data.sign_nonce_tracker.read().await.get(&address).copied();
    Ok(HttpResponse::Ok().json(nonce))
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
    Evicted,
    Pending,
    InPendingTx { tx_hash: TxHash },
    InConfirmedTx { tx_hash: TxHash, block_hash: H256 },
}

impl DataIntentStatus {
    pub fn is_known(&self) -> bool {
        match self {
            DataIntentStatus::InConfirmedTx { .. }
            | DataIntentStatus::InPendingTx { .. }
            | DataIntentStatus::Pending
            | DataIntentStatus::Evicted => true,
            DataIntentStatus::Unknown => false,
        }
    }

    pub fn is_in_tx(&self) -> Option<TxHash> {
        match self {
            DataIntentStatus::Unknown | DataIntentStatus::Evicted | DataIntentStatus::Pending => {
                None
            }
            DataIntentStatus::InPendingTx { tx_hash } => Some(*tx_hash),
            DataIntentStatus::InConfirmedTx { tx_hash, .. } => Some(*tx_hash),
        }
    }

    pub fn is_in_block(&self) -> Option<(H256, TxHash)> {
        match self {
            DataIntentStatus::Unknown
            | DataIntentStatus::Evicted
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

impl AppData {
    #[tracing::instrument(skip(self))]
    async fn balance_of_user(&self, from: &Address) -> i128 {
        self.sync.read().await.balance_with_pending(from)
            - self
                .data_intent_tracker
                .read()
                .await
                .pending_intents_total_cost(from) as i128
    }
}
