use actix_web::{get, web, HttpResponse};
use ethers::types::TxHash;
use serde::Serialize;
use std::sync::Arc;

use crate::utils::{e400, e500};
use crate::AppData;

#[derive(Serialize, Debug, Clone)]
pub struct BlobsResponse {
    pub tx_hash: TxHash,
    pub participants: Vec<BlobParticipantData>,
}

#[derive(Serialize, Debug, Clone)]
pub struct BlobParticipantData {
    pub address: String,
    pub data_len: usize,
    #[serde(with = "serde_utils::hex_vec")]
    pub data: Vec<u8>,
}

#[get("/v1/blobs/{tx_hash}")]
pub(crate) async fn get_blobs_by_tx_hash(
    data: web::Data<Arc<AppData>>,
    tx_hash: web::Path<TxHash>,
) -> Result<HttpResponse, actix_web::Error> {
    let tx_hash = tx_hash.into_inner();

    // Check cache first
    {
        let cache = data.blob_cache.read().await;
        if let Some(cached) = cache.get(&tx_hash) {
            return Ok(HttpResponse::Ok().json(cached));
        }
    }

    let consumer = data
        .beacon_consumer
        .as_ref()
        .ok_or_else(|| eyre::eyre!("beacon API not configured"))
        .map_err(e400)?;

    let response = consumer
        .fetch_blob_data_by_tx_hash(tx_hash)
        .await
        .map_err(e500)?;

    // Cache the result (blob data for included txs is immutable)
    {
        let mut cache = data.blob_cache.write().await;
        cache.insert(tx_hash, response.clone());
    }

    Ok(HttpResponse::Ok().json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blobs_response_serialization() {
        let response = BlobsResponse {
            tx_hash: TxHash::zero(),
            participants: vec![BlobParticipantData {
                address: "0x0000000000000000000000000000000000000001".to_string(),
                data_len: 3,
                data: vec![0xaa, 0xbb, 0xcc],
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(
            json["tx_hash"],
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(json["participants"][0]["data_len"], 3);
        assert_eq!(json["participants"][0]["data"], "0xaabbcc");
        assert_eq!(
            json["participants"][0]["address"],
            "0x0000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn blobs_response_empty_participants() {
        let response = BlobsResponse {
            tx_hash: TxHash::zero(),
            participants: vec![],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert!(json["participants"].as_array().unwrap().is_empty());
    }

    #[test]
    fn blob_participant_data_serialization_empty_data() {
        let participant = BlobParticipantData {
            address: "0x0000000000000000000000000000000000000042".to_string(),
            data_len: 0,
            data: vec![],
        };
        let json = serde_json::to_value(&participant).unwrap();
        assert_eq!(json["data_len"], 0);
        assert_eq!(json["data"], "0x");
    }
}
