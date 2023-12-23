use actix_web::{post, web, HttpResponse};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::{Address, Signature};
use eyre::{bail, eyre, Result};
use log::debug;
use num_traits::cast::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use sqlx::types::BigDecimal;
use sqlx::MySqlPool;
use std::sync::Arc;

use crate::client::DataIntentId;
use crate::data_intent::{BlobGasPrice, DataHash, DataIntent, DataIntentNoSignature};
use crate::data_intent_tracker::store_data_intent;
use crate::utils::{
    address_to_hex_lowercase, deserialize_signature, e400, e500, unix_timestamps_millis,
};
use crate::AppData;

#[tracing::instrument(skip(body, data), err)]
#[post("/v1/data")]
pub(crate) async fn post_data(
    body: web::Json<PostDataIntentV1Signed>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    // .try_into() verifies the signature
    let nonce = body.nonce;
    let data_intent: DataIntent = body.into_inner().try_into().map_err(e400)?;
    let onchain_balance = data.balance_of_user(data_intent.from()).await;

    let from = *data_intent.from();
    let data_len = data_intent.data_len();

    let id = atomic_update_post_data_on_unsafe_channel(
        &data.db_pool,
        data_intent,
        nonce,
        onchain_balance,
    )
    .await
    .map_err(e500)?;

    debug!("accepted data intent from {from} nonce {nonce} data_len {data_len} id {id}");

    // Potentially send a blob transaction including this new participation
    data.notify.notify_one();

    Ok(HttpResponse::Ok().json(PostDataResponse { id }))
}

#[tracing::instrument(skip(db_pool, data_intent))]
pub async fn atomic_update_post_data_on_unsafe_channel(
    db_pool: &MySqlPool,
    data_intent: DataIntent,
    nonce: u64,
    onchain_balance: i128,
) -> Result<DataIntentId> {
    let cost = data_intent.max_cost() as i128;
    let eth_address = address_to_hex_lowercase(*data_intent.from());

    let mut tx = db_pool.begin().await?;

    // Fetch user row, may not have any records yet
    let user_row = sqlx::query!(
        "SELECT total_data_intent_cost, post_data_nonce FROM users WHERE eth_address = ? FOR UPDATE",
        eth_address,
    )
    .fetch_optional(&mut *tx)
    .await?;

    // Check user balance
    let total_data_intent_cost = match &user_row {
        Some(row) => row
            .total_data_intent_cost
            .to_i128()
            .ok_or_else(|| eyre!("invalid db value total_data_intent_cost"))?,
        None => 0,
    };
    let last_nonce = user_row.and_then(|row| row.post_data_nonce);
    let balance = onchain_balance - total_data_intent_cost;

    if balance < cost {
        bail!("Insufficient balance");
    }

    // Check nonce is higher
    if let Some(last_nonce) = last_nonce {
        if nonce <= last_nonce.try_into()? {
            bail!("Nonce not new, replay protection");
        }
    }

    let new_total_data_intent_cost = BigDecimal::from_i128(total_data_intent_cost + cost);

    // Update balance and nonce
    // TODO: Should assert that 1 row was affected?
    sqlx::query!(
        "UPDATE users SET total_data_intent_cost = ?, post_data_nonce = ? WHERE eth_address = ?",
        new_total_data_intent_cost,
        Some(nonce),
        eth_address,
    )
    .execute(&mut *tx)
    .await?;

    let id = store_data_intent(&mut tx, data_intent).await?;

    // Commit transaction
    tx.commit().await?;

    Ok(id)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataResponse {
    pub id: DataIntentId,
}

/// TODO: Expose a "login with Ethereum" function an expose the non-signed variant
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1 {
    /// Address sending the data
    pub from: Address,
    /// Data to be posted
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>,
    /// Max price user is willing to pay in wei
    pub max_blob_gas_price: BlobGasPrice,
}

/// PostDataIntent message for non authenticated channels
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1Signed {
    pub intent: PostDataIntentV1,
    /// DataIntent nonce, to allow replay protection. Each new intent must have a nonce higher than
    /// the last known nonce from this `from` sender. Re-pricings will be done with a different
    /// nonce. For simplicity just pick the current UNIX timestemp in miliseconds.
    ///
    /// u64::MAX is 18446744073709551616, able to represent unix timestamps in miliseconds way into
    /// the future.
    pub nonce: u64,
    /// Signature over := data | nonce | max_blob_gas_price
    #[serde(with = "hex_vec")]
    pub signature: Vec<u8>,
}

impl PostDataIntentV1Signed {
    pub async fn with_signature(
        wallet: &LocalWallet,
        intent: PostDataIntentV1,
        nonce: Option<u64>,
    ) -> Result<Self> {
        if wallet.address() != intent.from {
            bail!(
                "intent.from {} does not match wallet address {}",
                intent.from,
                wallet.address()
            );
        }

        let nonce = nonce.unwrap_or_else(unix_timestamps_millis);
        let signature: Signature = wallet.sign_message(Self::sign_hash(&intent, nonce)).await?;

        Ok(Self {
            intent,
            nonce,
            signature: signature.into(),
        })
    }

    fn sign_hash(intent: &PostDataIntentV1, nonce: u64) -> Vec<u8> {
        let data_hash = DataHash::from_data(&intent.data);

        // Concat: data_hash | nonce | max_blob_gas_price
        let mut signed_data = data_hash.to_vec();
        signed_data.extend_from_slice(&intent.max_blob_gas_price.to_be_bytes());
        signed_data.extend_from_slice(&nonce.to_be_bytes());

        signed_data
    }

    fn verify_signature(&self) -> Result<()> {
        let signature = deserialize_signature(&self.signature)?;
        let sign_hash = PostDataIntentV1Signed::sign_hash(&self.intent, self.nonce);
        signature.verify(sign_hash, self.intent.from)?;
        Ok(())
    }
}

impl TryInto<DataIntent> for PostDataIntentV1Signed {
    type Error = eyre::Report;

    fn try_into(self) -> Result<DataIntent, Self::Error> {
        self.verify_signature()?;

        Ok(self.intent.into())
    }
}

impl From<PostDataIntentV1> for DataIntent {
    fn from(val: PostDataIntentV1) -> Self {
        let data_hash = DataHash::from_data(&val.data);
        Self::NoSignature(DataIntentNoSignature {
            from: val.from,
            data: val.data,
            data_hash,
            max_blob_gas_price: val.max_blob_gas_price,
        })
    }
}

impl From<DataIntent> for PostDataIntentV1 {
    fn from(value: DataIntent) -> Self {
        match value {
            DataIntent::NoSignature(d) => Self {
                from: d.from,
                data: d.data,
                max_blob_gas_price: d.max_blob_gas_price,
            },
            DataIntent::WithSignature(d) => Self {
                from: d.from,
                data: d.data,
                max_blob_gas_price: d.max_blob_gas_price,
            },
        }
    }
}

#[cfg(test)]
mod tests {

    use ethers::signers::{LocalWallet, Signer};
    use ethers::types::Address;
    use eyre::Result;
    use std::str::FromStr;

    use super::*;

    #[test]
    fn route_post_data_intent_v1_serde() -> Result<()> {
        let data_intent = PostDataIntentV1 {
            from: Address::from_str("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")?,
            data: vec![0xaa; 50],
            max_blob_gas_price: 1000000000,
        };

        assert_eq!(&serde_json::to_string(&data_intent)?, "{\"from\":\"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\",\"data\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"max_blob_gas_price\":1000000000}");

        Ok(())
    }

    #[tokio::test]
    async fn data_intent_signature_valid() -> Result<()> {
        let data = vec![0xaa; 50];
        let wallet = get_wallet()?;
        let intent = PostDataIntentV1 {
            from: wallet.address(),
            data,
            max_blob_gas_price: 1000,
        };
        let post_data_intent_signed =
            PostDataIntentV1Signed::with_signature(&&wallet, intent, None).await?;

        post_data_intent_signed.verify_signature()?;

        let _recovered_intent: DataIntent = post_data_intent_signed.try_into()?;

        Ok(())
    }

    #[tokio::test]
    async fn data_intent_signature_invalid() -> Result<()> {
        let data = vec![0xaa; 50];
        let wallet = get_wallet()?;
        let intent = PostDataIntentV1 {
            from: wallet.address(),
            data,
            max_blob_gas_price: 1000,
        };
        let mut post_data_intent_signed =
            PostDataIntentV1Signed::with_signature(&&wallet, intent, None).await?;
        post_data_intent_signed.intent.from = [0; 20].into();

        assert_eq!(
            post_data_intent_signed
                .verify_signature()
                .unwrap_err()
                .to_string(),
            "Signature verification failed. Expected 0x0000…0000, got 0xdbd4…3277"
        );

        assert_eq!(
            TryInto::<DataIntent>::try_into(post_data_intent_signed)
                .unwrap_err()
                .to_string(),
            "Signature verification failed. Expected 0x0000…0000, got 0xdbd4…3277"
        );

        Ok(())
    }

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";
    const DEV_PUBKEY: &str = "0xdbD48e742FF3Ecd3Cb2D557956f541b6669b3277";

    pub fn get_wallet() -> Result<LocalWallet> {
        let wallet = LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?;
        assert_eq!(wallet.address(), Address::from_str(DEV_PUBKEY)?);
        Ok(wallet)
    }
}
