use actix_web::{
    dev::{ServiceFactory, ServiceRequest},
    web, HttpResponse,
};
use bundler_client::types::DataIntentId;
use ethers::types::{Address, TxHash};
use serde_json::json;
use std::sync::Arc;

use crate::{
    utils::{e500, vec_to_hex_0x_prefix},
    AppData, BlobGasPrice,
};

/// Default number of recent blob TXs shown on the home page.
const HOME_RECENT_TX_LIMIT: u32 = 20;
/// Default history page size for the address explorer page.
const ADDRESS_HISTORY_LIMIT: u32 = 50;

pub(crate) fn register_explorer_service<
    T: ServiceFactory<ServiceRequest, Config = (), Error = actix_web::Error, InitError = ()>,
>(
    app: actix_web::App<T>,
) -> actix_web::App<T> {
    app.route("/", web::get().to(get_home))
        .route("/address/{address}", web::get().to(get_address))
        .route("/intent/{id}", web::get().to(get_intent))
        .route("/tx/{tx_hash}", web::get().to(get_tx))
        .service(actix_files::Files::new("/static", "./static").show_files_listing())
}

pub(crate) async fn get_home(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let head_gas = data.get_head_gas().await;

    let (mut data_intents, _) = data
        .get_all_intents_available_for_packing(BlobGasPrice::MIN)
        .await;

    data_intents.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    // Fetch recent published blob transactions
    let recent_txs = data
        .recent_blob_txs(HOME_RECENT_TX_LIMIT)
        .await
        .unwrap_or_default();

    let recent_txs_json: Vec<_> = recent_txs
        .iter()
        .map(|row| {
            json!({
                "tx_hash": vec_to_hex_0x_prefix(&row.tx_hash),
                "participant_count": row.participant_count,
                "total_data_len": row.total_data_len,
                "updated_at": row.latest_update.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
            })
        })
        .collect();

    let values = json!({
        "data_intents": data_intents,
        "recent_txs": recent_txs_json,
        "blob_gas_price": head_gas.blob_gas_price(),
        "base_fee_per_gas": head_gas.base_fee_per_gas,
    });
    let body = data.handlebars.render("index", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

pub(crate) async fn get_address(
    data: web::Data<Arc<AppData>>,
    path: web::Path<Address>,
    query: web::Query<AddressPageQuery>,
) -> Result<HttpResponse, actix_web::Error> {
    let address = path.into_inner();
    let balance_wei = data.balance_of_user(&address).await;
    let balance_eth = (balance_wei / 1_000_000_000) as f64 / 1_000_000_000.;

    let offset = query.offset.unwrap_or(0);
    let limit = query
        .limit
        .unwrap_or(ADDRESS_HISTORY_LIMIT)
        .min(ADDRESS_HISTORY_LIMIT);

    let history = data
        .history_for_address(&address, limit, offset)
        .await
        .map_err(e500)?;

    let intents_json: Vec<_> = history
        .entries
        .iter()
        .map(|entry| {
            let (status, status_class, tx_hash) = match &entry.status {
                bundler_client::types::HistoryEntryStatus::Pending => {
                    ("Pending".to_string(), "status-pending", None)
                }
                bundler_client::types::HistoryEntryStatus::Cancelled => {
                    ("Cancelled".to_string(), "status-cancelled", None)
                }
                bundler_client::types::HistoryEntryStatus::Included { tx_hash } => (
                    "Included".to_string(),
                    "status-included",
                    Some(format!("{tx_hash:?}")),
                ),
                bundler_client::types::HistoryEntryStatus::Finalized { tx_hash } => (
                    "Finalized".to_string(),
                    "status-finalized",
                    Some(format!("{tx_hash:?}")),
                ),
            };
            json!({
                "id": entry.id,
                "data_len": entry.data_len,
                "data_hash": vec_to_hex_0x_prefix(&entry.data_hash),
                "max_blob_gas_price": entry.max_blob_gas_price,
                "status": status,
                "status_class": status_class,
                "tx_hash": tx_hash,
                "updated_at": entry.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            })
        })
        .collect();

    let has_prev = offset > 0;
    let has_next = (offset as u64 + limit as u64) < history.total;

    let values = json!({
        "address": address,
        "balance": balance_eth,
        "intents": intents_json,
        "total": history.total,
        "offset": offset,
        "limit": limit,
        "has_prev": has_prev,
        "has_next": has_next,
        "prev_offset": offset.saturating_sub(limit),
        "next_offset": offset + limit,
    });
    let body = data.handlebars.render("address", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

pub(crate) async fn get_intent(
    data: web::Data<Arc<AppData>>,
    id: web::Path<DataIntentId>,
) -> Result<HttpResponse, actix_web::Error> {
    let item = data.data_intent_by_id(&id).await.map_err(e500)?;
    let status = data.status_by_id(&id).await.map_err(e500)?;

    let (status_text, status_class, tx_hash) = match &status {
        bundler_client::types::DataIntentStatus::Unknown => {
            ("Unknown".to_string(), "status-pending", None)
        }
        bundler_client::types::DataIntentStatus::Pending => {
            ("Pending".to_string(), "status-pending", None)
        }
        bundler_client::types::DataIntentStatus::Cancelled => {
            ("Cancelled".to_string(), "status-cancelled", None)
        }
        bundler_client::types::DataIntentStatus::InPendingTx { tx_hash } => (
            "In pending TX".to_string(),
            "status-included",
            Some(format!("{tx_hash:?}")),
        ),
        bundler_client::types::DataIntentStatus::InConfirmedTx {
            tx_hash,
            block_hash: _,
        } => (
            "Confirmed".to_string(),
            "status-finalized",
            Some(format!("{tx_hash:?}")),
        ),
    };

    let values = json!({
        "id": *id,
        "from": vec_to_hex_0x_prefix(&item.eth_address),
        "data_len": item.data_len,
        "data_hash": vec_to_hex_0x_prefix(&item.data_hash),
        "data": vec_to_hex_0x_prefix(&item.data),
        "max_blob_gas_price": item.max_blob_gas_price,
        "status": status_text,
        "status_class": status_class,
        "tx_hash": tx_hash,
    });
    let body = data.handlebars.render("intent", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

pub(crate) async fn get_tx(
    data: web::Data<Arc<AppData>>,
    tx_hash_path: web::Path<TxHash>,
) -> Result<HttpResponse, actix_web::Error> {
    let tx_hash = tx_hash_path.into_inner();

    // Try to fetch blob data if beacon consumer is available
    let blob_data = if let Some(consumer) = &data.beacon_consumer {
        // Check cache first
        let cached = {
            let cache = data.blob_cache.read().await;
            cache.get(&tx_hash).cloned()
        };
        if cached.is_some() {
            cached
        } else {
            match consumer.fetch_blob_data_by_tx_hash(tx_hash).await {
                Ok(response) => {
                    let mut cache = data.blob_cache.write().await;
                    cache.insert(tx_hash, response.clone());
                    Some(response)
                }
                Err(_) => None,
            }
        }
    } else {
        None
    };

    let participants_json: Vec<_> = blob_data
        .as_ref()
        .map(|bd| {
            bd.participants
                .iter()
                .map(|p| {
                    json!({
                        "address": p.address,
                        "data_len": p.data_len,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let total_data: usize = blob_data
        .as_ref()
        .map(|bd| bd.participants.iter().map(|p| p.data_len).sum())
        .unwrap_or(0);

    let values = json!({
        "tx_hash": format!("{tx_hash:?}"),
        "participants": participants_json,
        "participant_count": participants_json.len(),
        "total_data_len": total_data,
        "has_blob_data": blob_data.is_some(),
    });
    let body = data.handlebars.render("tx", &values).map_err(e500)?;

    Ok(HttpResponse::Ok().body(body))
}

#[derive(serde::Deserialize)]
pub(crate) struct AddressPageQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_page_query_defaults() {
        let q: AddressPageQuery = serde_json::from_str("{}").unwrap();
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
    }

    #[test]
    fn address_page_query_with_values() {
        let q: AddressPageQuery = serde_json::from_str(r#"{"limit": 10, "offset": 20}"#).unwrap();
        assert_eq!(q.limit, Some(10));
        assert_eq!(q.offset, Some(20));
    }

    #[test]
    fn address_limit_clamped() {
        let requested = 500u32;
        let clamped = requested.min(ADDRESS_HISTORY_LIMIT);
        assert_eq!(clamped, ADDRESS_HISTORY_LIMIT);
    }

    #[test]
    fn address_limit_within_max_unchanged() {
        let requested = 25u32;
        let clamped = requested.min(ADDRESS_HISTORY_LIMIT);
        assert_eq!(clamped, 25);
    }

    #[test]
    fn home_recent_tx_limit_positive() {
        assert!(HOME_RECENT_TX_LIMIT > 0);
    }
}
