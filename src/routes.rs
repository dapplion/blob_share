use actix_web::{get, post, web, HttpResponse, Responder};
use hex;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;

use crate::{compute_data_hash, verify_account_balance, AppData, DataIntent};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct PostDataIntent {
    /// Address sending the data
    pub(crate) from: String,
    #[serde(
        serialize_with = "serialize_as_hex",
        deserialize_with = "deserialize_from_hex"
    )]
    /// Data to be posted
    pub(crate) data: Vec<u8>,
    /// Max price user is willing to pay in wei
    pub(crate) max_price: u64,
}

#[get("/health")]
pub(crate) async fn get_status() -> impl Responder {
    HttpResponse::Ok().finish()
}

#[post("/data")]
pub(crate) async fn post_data(
    item: web::Json<PostDataIntent>,
    data: web::Data<Arc<AppData>>,
) -> impl Responder {
    if !verify_account_balance(&item.from).await {
        return HttpResponse::BadRequest().body("Insufficient balance");
    }

    // Add data intent to queue and notify background task
    let data_hash = compute_data_hash(&item.data);

    match data
        .queue
        .lock()
        .unwrap()
        .entry((item.from.clone(), data_hash))
    {
        Entry::Vacant(entry) => entry.insert(DataIntent {
            from: item.from.clone(),
            data: item.data.clone(),
            max_price: item.max_price,
            data_hash,
        }),
        Entry::Occupied(_) => {
            return HttpResponse::InternalServerError().body("data intent already known")
        }
    };

    data.notify.notify_one();

    HttpResponse::Ok().finish()
}

#[get("/data")]
pub(crate) async fn get_data(data: web::Data<Arc<AppData>>) -> impl Responder {
    let items = {
        data.queue
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<DataIntent>>()
    };

    HttpResponse::Ok().json(
        items
            .into_iter()
            .map(|v| v.into())
            .collect::<Vec<PostDataIntent>>(),
    )
}

fn serialize_as_hex<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let hex_string = format!("0x{}", hex::encode(bytes));
    serializer.serialize_str(&hex_string)
}

fn deserialize_from_hex<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct HexVisitor;

    impl<'de> Visitor<'de> for HexVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a hex string with a 0x prefix")
        }

        fn visit_str<E>(self, value: &str) -> Result<Vec<u8>, E>
        where
            E: de::Error,
        {
            if value.starts_with("0x") || value.starts_with("0X") {
                hex::decode(&value[2..]).map_err(E::custom)
            } else {
                Err(E::custom("Expected a hex string with a 0x prefix"))
            }
        }
    }

    deserializer.deserialize_str(HexVisitor)
}

impl From<DataIntent> for PostDataIntent {
    fn from(value: DataIntent) -> Self {
        Self {
            from: value.from,
            data: value.data,
            max_price: value.max_price,
        }
    }
}
