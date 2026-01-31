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
    let max_data_size = data.config.max_data_size;

    // Reject data larger than the configured maximum
    if data_len > max_data_size {
        return Err(e400(eyre!(
            "data length {} over max data size {}",
            data_len,
            max_data_size
        )));
    }

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

    // If data fits in a single blob, store as a single intent (backward compatible)
    if data_len <= MAX_USABLE_BLOB_DATA_LEN {
        let id = data
            .atomic_update_post_data_on_unsafe_channel(data_intent, nonce)
            .await
            .map_err(e500)?;

        debug!("accepted data intent from {from} nonce {nonce} data_len {data_len} id {id}");

        data.notify.notify_one();

        return Ok(HttpResponse::Ok().json(PostDataResponse {
            id,
            group_id: None,
            chunk_ids: None,
        }));
    }

    // Split data across multiple blob-sized chunks
    let chunks = split_into_chunks(data_intent);

    let chunk_count = chunks.len();
    let (group_id, chunk_ids) = data
        .atomic_update_post_data_chunks(chunks, nonce)
        .await
        .map_err(e500)?;

    debug!(
        "accepted multi-blob data intent from {from} nonce {nonce} data_len {data_len} \
         group_id {group_id} chunks {chunk_count}"
    );

    data.notify.notify_one();

    // Return the first chunk ID as the primary `id` for backward compatibility
    Ok(HttpResponse::Ok().json(PostDataResponse {
        id: chunk_ids[0],
        group_id: Some(group_id),
        chunk_ids: Some(chunk_ids),
    }))
}

/// Split a data intent into blob-sized chunks, each as a separate `DataIntent`.
/// Each chunk inherits the same `from`, `max_blob_gas_price`, and signature info.
pub(crate) fn split_into_chunks(data_intent: DataIntent) -> Vec<DataIntent> {
    let from = *data_intent.from();
    let max_blob_gas_price = data_intent.max_blob_gas_price();
    let full_data = data_intent.data().to_vec();

    full_data
        .chunks(MAX_USABLE_BLOB_DATA_LEN)
        .map(|chunk| {
            let chunk_data = chunk.to_vec();
            let data_hash = DataHash::from_data(&chunk_data);
            DataIntent::NoSignature(DataIntentNoSignature {
                from,
                data: chunk_data,
                data_hash,
                max_blob_gas_price,
            })
        })
        .collect()
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

    fn make_intent(data: Vec<u8>, max_blob_gas_price: u64) -> DataIntent {
        DataIntent::NoSignature(DataIntentNoSignature {
            from: Address::zero(),
            data_hash: DataHash::from_data(&data),
            data,
            max_blob_gas_price,
        })
    }

    #[test]
    fn split_into_chunks_single_blob_returns_one_chunk() {
        let data = vec![0xab; MAX_USABLE_BLOB_DATA_LEN];
        let intent = make_intent(data.clone(), 1000);
        let chunks = split_into_chunks(intent);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data(), data.as_slice());
        assert_eq!(chunks[0].max_blob_gas_price(), 1000);
        assert_eq!(*chunks[0].from(), Address::zero());
    }

    #[test]
    fn split_into_chunks_just_over_one_blob_returns_two_chunks() {
        let data = vec![0xcd; MAX_USABLE_BLOB_DATA_LEN + 1];
        let intent = make_intent(data.clone(), 5000);
        let chunks = split_into_chunks(intent);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].data_len(), MAX_USABLE_BLOB_DATA_LEN);
        assert_eq!(chunks[1].data_len(), 1);
        // Verify data integrity: concatenating chunks yields original data
        let mut reassembled = chunks[0].data().to_vec();
        reassembled.extend_from_slice(chunks[1].data());
        assert_eq!(reassembled, data);
    }

    #[test]
    fn split_into_chunks_exact_multiple_of_blob_size() {
        let data = vec![0xef; MAX_USABLE_BLOB_DATA_LEN * 3];
        let intent = make_intent(data.clone(), 2000);
        let chunks = split_into_chunks(intent);

        assert_eq!(chunks.len(), 3);
        for chunk in &chunks {
            assert_eq!(chunk.data_len(), MAX_USABLE_BLOB_DATA_LEN);
            assert_eq!(chunk.max_blob_gas_price(), 2000);
            assert_eq!(*chunk.from(), Address::zero());
        }
        let reassembled: Vec<u8> = chunks.iter().flat_map(|c| c.data().to_vec()).collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn split_into_chunks_preserves_data_hashes() {
        let data = vec![0x42; MAX_USABLE_BLOB_DATA_LEN * 2 + 100];
        let intent = make_intent(data.clone(), 1000);
        let chunks = split_into_chunks(intent);

        assert_eq!(chunks.len(), 3);
        // Each chunk's data_hash should match its own data, not the full data
        for chunk in &chunks {
            let expected_hash = DataHash::from_data(chunk.data());
            assert_eq!(chunk.data_hash(), &expected_hash);
        }
    }

    #[test]
    fn split_into_chunks_max_data_size_six_blobs() {
        // Default max: 6 blobs worth of data
        let data = vec![0x11; MAX_USABLE_BLOB_DATA_LEN * 6];
        let intent = make_intent(data, 500);
        let chunks = split_into_chunks(intent);

        assert_eq!(chunks.len(), 6);
        for chunk in &chunks {
            assert_eq!(chunk.data_len(), MAX_USABLE_BLOB_DATA_LEN);
        }
    }

    #[test]
    fn post_data_response_single_blob_serde_backward_compatible() {
        let id = uuid::Uuid::new_v4();
        let response = PostDataResponse {
            id,
            group_id: None,
            chunk_ids: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        // Verify that group_id and chunk_ids are omitted (backward compatible)
        assert!(!json.contains("group_id"));
        assert!(!json.contains("chunk_ids"));
        // Can be deserialized back
        let deserialized: PostDataResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, id);
        assert!(deserialized.group_id.is_none());
        assert!(deserialized.chunk_ids.is_none());
    }

    #[test]
    fn post_data_response_multi_blob_serde() {
        let id = uuid::Uuid::new_v4();
        let group_id = uuid::Uuid::new_v4();
        let chunk_id_2 = uuid::Uuid::new_v4();
        let response = PostDataResponse {
            id,
            group_id: Some(group_id),
            chunk_ids: Some(vec![id, chunk_id_2]),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("group_id"));
        assert!(json.contains("chunk_ids"));
        let deserialized: PostDataResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.group_id.unwrap(), group_id);
        assert_eq!(deserialized.chunk_ids.unwrap().len(), 2);
    }
}
