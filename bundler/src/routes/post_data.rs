use actix_web::{post, web, HttpResponse};
use bundler_client::types::{DataHash, PostDataIntentV1, PostDataIntentV1Signed, PostDataResponse};
use eyre::{eyre, Result};
use log::debug;
use std::sync::Arc;

use crate::data_intent::{DataIntent, DataIntentNoSignature};
use crate::utils::{e400, e500};
use crate::{AppData, MAX_PENDING_DATA_LEN_PER_USER, MAX_USABLE_BLOB_DATA_LEN};

#[tracing::instrument(skip(body, data), err)]
#[post("/v1/data")]
pub(crate) async fn post_data(
    body: web::Json<PostDataIntentV1Signed>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    // .try_into() verifies the signature
    let nonce = body.nonce;
    let data_intent: DataIntent = body.into_inner().try_into().map_err(e400)?;
    let from = *data_intent.from();
    let data_len = data_intent.data_len();

    // TODO: Consider support for splitting data over mutliple blobs
    if data_intent.data_len() > MAX_USABLE_BLOB_DATA_LEN {
        return Err(e400(eyre!(
            "data length {} over max usable blob data {}",
            data_intent.data_len(),
            MAX_USABLE_BLOB_DATA_LEN
        )));
    }

    // TODO: Is this limitation necessary?
    let pending_total_data_len = data.pending_total_data_len(&from).await;
    if pending_total_data_len + data_len > MAX_PENDING_DATA_LEN_PER_USER {
        return Err(e400(eyre!(
            "pending total data_len {} over max {}",
            pending_total_data_len + data_len,
            MAX_PENDING_DATA_LEN_PER_USER
        )));
    }

    // TODO: Review the cost of sync here time
    data.sync_data_intents().await.map_err(e500)?;
    let balance = data.balance_of_user(&from).await;
    let cost = data_intent.max_cost() as i128;
    if balance < cost {
        return Err(e400(eyre!(
            "Insufficient balance {balance} for intent with cost {cost}"
        )));
    }

    let id = data
        .atomic_update_post_data_on_unsafe_channel(data_intent, nonce)
        .await
        .map_err(e500)?;

    debug!("accepted data intent from {from} nonce {nonce} data_len {data_len} id {id}");

    // Potentially send a blob transaction including this new participation
    data.notify.notify_one();

    Ok(HttpResponse::Ok().json(PostDataResponse { id }))
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
