use actix_web::{get, web, HttpResponse, Responder};
use bundler_client::types::{
    BlockGasSummary, DataIntentFull, DataIntentId, DataIntentStatus, DataIntentSummary,
    SenderDetails, SyncStatus, SyncStatusBlock,
};
use ethers::signers::Signer;
use ethers::types::Address;
use eyre::{eyre, Result};
use std::sync::Arc;

pub mod delete_data;
pub mod get_blobs;
pub mod get_history;
pub mod post_data;

use crate::eth_provider::EthProvider;
use crate::utils::e500;
use crate::{AppData, BlobGasPrice};

#[get("/v1/health")]
pub(crate) async fn get_health(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    data.health_check().await.map_err(e500)?;
    Ok(HttpResponse::Ok().finish())
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

#[get("/v1/gas")]
pub(crate) async fn get_gas(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let head_gas: BlockGasSummary = data.get_head_gas().await;
    Ok(HttpResponse::Ok().json(head_gas))
}

// #[post("/v1/data")}
// post_data
// > MOVED to routes/post_data.rs

#[get("/v1/data")]
pub(crate) async fn get_data(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let items: Vec<DataIntentSummary> = data
        .get_all_intents_available_for_packing(BlobGasPrice::MIN)
        .await
        .0;
    Ok(HttpResponse::Ok().json(items))
}

#[get("/v1/data/{id}")]
pub(crate) async fn get_data_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<DataIntentId>,
) -> Result<HttpResponse, actix_web::Error> {
    // TODO: Try to unify types, too many `DataIntent*` things
    let item: DataIntentFull = data.data_intent_by_id(&id).await.map_err(e500)?;
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
