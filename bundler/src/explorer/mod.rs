use actix_web::{get, web, HttpResponse, Responder};
use ethers::types::Address;
use serde_json::json;
use std::sync::Arc;

use crate::{AppData, BlobGasPrice};

// TODO: Add route to cancel data intents by ID

#[get("/")]
pub(crate) async fn get_home(data: web::Data<Arc<AppData>>) -> impl Responder {
    let head_gas = data.get_head_gas().await;

    let (mut data_intents, _) = data
        .get_all_intents_available_for_packing(BlobGasPrice::MAX)
        .await;

    data_intents.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let values = json!({
        "data_intents": data_intents,
        "blob_gas_price": head_gas.blob_gas_price(),
        "base_fee_per_gas": head_gas.base_fee_per_gas,
    });
    let body = data.handlebars.render("index", &values).unwrap();

    HttpResponse::Ok().body(body)
}

#[get("/address/{address}")]
pub(crate) async fn get_address(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
) -> impl Responder {
    let balance_wei = data.balance_of_user(&address).await;
    let balance_eth = (balance_wei / 1_000_000_000) as f64 / 1_000_000_000.;

    let values = json!({
        "address": *address,
        "balance": balance_eth,
    });
    let body = data.handlebars.render("address", &values).unwrap();

    HttpResponse::Ok().body(body)
}
