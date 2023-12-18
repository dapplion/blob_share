use actix_web::{post, web, HttpResponse};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::{Address, Signature};
use eyre::{bail, eyre, Result};
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use std::sync::Arc;

use crate::data_intent::{DataHash, DataIntent, DataIntentNoSignature};
use crate::utils::{deserialize_signature, e400, e500};
use crate::AppData;

#[post("/v1/data")]
pub(crate) async fn post_data(
    body: web::Json<PostDataIntentV1Signed>,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    // .try_into() verifies the signature
    let data_intent: DataIntent = body.into_inner().try_into().map_err(e400)?;

    // Check that the nonce is the next expected
    let expected_next_nonce = data.nonce_of_user(data_intent.from()).await;
    if expected_next_nonce != data_intent.nonce() {
        return Err(e400(eyre!(
            "incorrect nonce {} expected {}",
            data_intent.nonce(),
            expected_next_nonce
        )));
    }

    // Check user has enough balance to cover the max cost allowed
    let balance = data.balance_of_user(data_intent.from()).await;
    if balance < data_intent.max_cost() as i128 {
        return Err(e400(eyre!(
            "Insufficient balance, current balance {} requested {}",
            balance,
            data_intent.max_cost()
        )));
    }

    // data_intent_tracker ensures no duplicates at this point, everything before this statement
    // must be immmutable checks
    let id = data_intent.id();
    data.data_intent_tracker
        .write()
        .await
        .add(data_intent)
        .map_err(e500)?;

    // Potentially send a blob transaction including this new participation
    data.notify.notify_one();

    Ok(HttpResponse::Ok().json(PostDataResponse { id: id.to_string() }))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataResponse {
    pub id: String,
}

/// TODO: Expose a "login with Ethereum" function an expose the non-signed variant
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1 {
    /// Address sending the data
    pub from: Address,
    /// Data to be posted
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>,
    /// DataIntent nonce, to allow replay protection. Each new intent must have a nonce higher than
    /// the last known nonce from this `from` sender. Re-pricings will be done with a different
    /// nonce
    pub nonce: u64,
    /// Max price user is willing to pay in wei
    pub max_blob_gas_price: u128,
}

/// PostDataIntent message for non authenticated channels
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1Signed {
    pub intent: PostDataIntentV1,
    /// Signature over [data,nonce,max_blob_gas_price]
    #[serde(with = "hex_vec")]
    pub signature: Vec<u8>,
}

impl PostDataIntentV1Signed {
    pub async fn with_signature(wallet: &LocalWallet, intent: PostDataIntentV1) -> Result<Self> {
        if wallet.address() != intent.from {
            bail!(
                "intent.from {} does not match wallet address {}",
                intent.from,
                wallet.address()
            );
        }

        let signature: Signature = wallet.sign_message(Self::sign_hash(&intent)).await?;

        Ok(Self {
            intent,
            signature: signature.into(),
        })
    }

    fn sign_hash(intent: &PostDataIntentV1) -> Vec<u8> {
        let data_hash = DataHash::from_data(&intent.data);

        // Concat: data_hash | nonce | max_blob_gas_price
        let mut signed_data = data_hash.to_vec();
        signed_data.extend_from_slice(&intent.nonce.to_be_bytes());
        signed_data.extend_from_slice(&intent.max_blob_gas_price.to_be_bytes());

        signed_data
    }

    fn verify_signature(&self) -> Result<()> {
        let signature = deserialize_signature(&self.signature)?;
        let sign_hash = PostDataIntentV1Signed::sign_hash(&self.intent);
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
            nonce: val.nonce,
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
                nonce: d.nonce,
                max_blob_gas_price: d.max_blob_gas_price,
            },
            DataIntent::WithSignature(d) => Self {
                from: d.from,
                data: d.data,
                nonce: d.nonce,
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
            nonce: 123,
            max_blob_gas_price: 1000000000,
        };

        assert_eq!(&serde_json::to_string(&data_intent)?, "{\"from\":\"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\",\"data\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"nonce\":123,\"max_blob_gas_price\":1000000000}");

        Ok(())
    }

    #[tokio::test]
    async fn data_intent_signature_valid() -> Result<()> {
        let data = vec![0xaa; 50];
        let wallet = get_wallet()?;
        let intent = PostDataIntentV1 {
            from: wallet.address(),
            data,
            nonce: 0,
            max_blob_gas_price: 1000,
        };
        let post_data_intent_signed =
            PostDataIntentV1Signed::with_signature(&&wallet, intent).await?;

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
            nonce: 0,
            max_blob_gas_price: 1000,
        };
        let mut post_data_intent_signed =
            PostDataIntentV1Signed::with_signature(&&wallet, intent).await?;
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
