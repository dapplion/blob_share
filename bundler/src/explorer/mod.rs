use actix_web::{
    dev::{ServiceFactory, ServiceRequest},
    web, HttpResponse,
};
use bundler_client::types::DataIntentId;
use ethers::types::Address;
use serde_json::json;
use std::sync::Arc;

use crate::{
    utils::{e500, vec_to_hex_0x_prefix},
    AppData, BlobGasPrice,
};

// TODO: Add route to cancel data intents by ID

pub(crate) fn register_explorer_service<
    T: ServiceFactory<ServiceRequest, Config = (), Error = actix_web::Error, InitError = ()>,
>(
    app: actix_web::App<T>,
) -> actix_web::App<T> {
    app.route("/", web::get().to(get_home))
        .route("/address/{address}", web::get().to(get_address))
        .route("/intent/{id}", web::get().to(get_intent))
}

pub(crate) async fn get_home(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let head_gas = data.get_head_gas().await;

    let (mut data_intents, _) = data
        .get_all_intents_available_for_packing(BlobGasPrice::MIN)
        .await;

    data_intents.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let values = json!({
        "data_intents": data_intents,
        "blob_gas_price": head_gas.blob_gas_price(),
        "base_fee_per_gas": head_gas.base_fee_per_gas,
    });
    let body = data.handlebars.render("index", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

pub(crate) async fn get_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> Result<HttpResponse, actix_web::Error> {
    let balance_wei = data.balance_of_user(&address).await;
    let balance_eth = (balance_wei / 1_000_000_000) as f64 / 1_000_000_000.;

    let values = json!({
        "address": *address,
        "balance": balance_eth,
    });
    let body = data.handlebars.render("address", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

pub(crate) async fn get_intent(
    data: web::Data<Arc<AppData>>,
    id: web::Path<DataIntentId>,
) -> Result<HttpResponse, actix_web::Error> {
    let item = data.data_intent_by_id(&id).await.map_err(e500)?;

    let values = json!({
        "id": *id,
        "from": vec_to_hex_0x_prefix(&item.eth_address),
        "data_len": item.data_len,
        "data_hash": vec_to_hex_0x_prefix(&item.data_hash),
        "data": vec_to_hex_0x_prefix(&item.data),
    });
    let body = data.handlebars.render("intent", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}
