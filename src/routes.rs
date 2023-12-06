use actix_web::{get, post, web, HttpResponse, Responder};
use ethers::signers::Signer;
use ethers::types::Address;
use eyre::eyre;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

use crate::data_intent::{deserialize_signature, DataHash, DataIntent, DataIntentId};
use crate::utils::{deserialize_from_hex, e400, e500, serialize_as_hex};
use crate::AppData;

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

#[post("/v1/data")]
pub(crate) async fn post_data(
    body: web::Json<PostDataIntentV1>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let data_intent: DataIntent = body.into_inner().try_into().map_err(e400)?;
    data_intent.verify_signature().map_err(e400)?;

    let account_balance = data.sync.unfinalized_balance_delta(data_intent.from).await;
    if account_balance < data_intent.max_cost_wei as i128 {
        return Err(e400(eyre!("Insufficient balance")));
    }

    let id = data
        .data_intent_tracker
        .add(data_intent)
        .await
        .map_err(e500)?;

    data.notify.notify_one();

    Ok(HttpResponse::Ok().json(PostDataResponse { id: id.to_string() }))
}

#[get("/v1/data")]
pub(crate) async fn get_data(
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    let items = { data.data_intent_tracker.get_all_pending().await };
    Ok(HttpResponse::Ok().json(items))
}

#[get("/v1/data/{id}")]
pub(crate) async fn get_data_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = DataIntentId::from_str(&id).map_err(e400)?;
    let item = {
        data.data_intent_tracker
            .data_by_id(&id)
            .await
            .ok_or_else(|| e400(format!("no item found for ID {}", id.to_string())))?
    };
    Ok(HttpResponse::Ok().json(item))
}

#[get("/v1/status/{id}")]
pub(crate) async fn get_status_by_id(
    data: web::Data<Arc<AppData>>,
    id: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let id = DataIntentId::from_str(&id).map_err(e400)?;
    let item = {
        data.data_intent_tracker
            .status_by_id(&data.sync, &id)
            .await
            .ok_or_else(|| e400(format!("no item found for ID {}", id.to_string())))?
    };
    Ok(HttpResponse::Ok().json(item))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SenderDetails {
    pub address: Address,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataResponse {
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1 {
    /// Address sending the data
    pub from: Address,
    #[serde(
        serialize_with = "serialize_as_hex",
        deserialize_with = "deserialize_from_hex"
    )]
    /// Data to be posted
    pub data: Vec<u8>,
    #[serde(
        serialize_with = "serialize_as_hex",
        deserialize_with = "deserialize_from_hex"
    )]
    pub signature: Vec<u8>,
    /// Max price user is willing to pay in wei
    pub max_cost_wei: u128,
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
            max_cost_wei: self.max_cost_wei,
        })
    }
}

impl From<DataIntent> for PostDataIntentV1 {
    fn from(value: DataIntent) -> Self {
        Self {
            from: value.from,
            data: value.data,
            signature: value.signature.to_vec(),
            max_cost_wei: value.max_cost_wei,
        }
    }
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
            max_cost_wei: 1000000000,
        };

        assert_eq!(&serde_json::to_string(&data_intent)?, "{\"from\":\"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\",\"data\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"signature\":\"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"max_cost_wei\":1000000000}");

        Ok(())
    }
}
