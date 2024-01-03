use ethers::{
    signers::{LocalWallet, Signer},
    types::Address,
};
use eyre::{Context, Result};
use url::Url;

use crate::{
    types::{
        BlobGasPrice, BlockGasSummary, DataIntentFull, DataIntentId, DataIntentStatus,
        DataIntentSummary, PostDataIntentV1, PostDataIntentV1Signed, PostDataResponse,
        SenderDetails, SyncStatus,
    },
    utils::{address_to_hex_lowercase, is_ok_response, unix_timestamps_millis},
};

pub struct Client {
    base_url: Url,
    client: reqwest::Client,
}

impl Client {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            base_url: Url::parse(base_url)?,
            client: reqwest::Client::new(),
        })
    }

    // Helper methods

    pub async fn post_data_with_wallet(
        &self,
        wallet: &LocalWallet,
        data: Vec<u8>,
        gas: &GasPreference,
        nonce: &NoncePreference,
    ) -> Result<PostDataResponse> {
        // TODO: customize, for now set gas price equal to next block
        // TODO: Close to genesis block the value is 1, which requires blobs to be perfectly full
        let max_blob_gas_price = match gas {
            GasPreference::RelativeToHead(factor) => {
                let head_gas = self.get_gas().await.wrap_err("get_gas")?;
                let blob_gas_price_next_block = head_gas.blob_gas_price_next_block();
                if *factor == 1.0 {
                    blob_gas_price_next_block as BlobGasPrice
                } else {
                    (((FACTOR_RESOLUTION as f64 * factor) as u128 * blob_gas_price_next_block)
                        / FACTOR_RESOLUTION) as BlobGasPrice
                }
            }
            GasPreference::Value(blob_gas_price) => *blob_gas_price,
        };

        let nonce = match nonce {
            NoncePreference::Timebased => unix_timestamps_millis(),
            NoncePreference::Value(nonce) => *nonce,
        };

        let intent_signed = PostDataIntentV1Signed::with_signature(
            wallet,
            PostDataIntentV1 {
                from: wallet.address(),
                data,
                max_blob_gas_price,
            },
            Some(nonce),
        )
        .await?;

        self.post_data(&intent_signed).await
    }

    // Exposed API routes

    pub async fn health(&self) -> Result<()> {
        let response = self.client.get(&self.url("v1/health")).send().await?;
        is_ok_response(response).await?;
        Ok(())
    }

    pub async fn get_sender(&self) -> Result<SenderDetails> {
        let response = self.client.get(&self.url("v1/sender")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_sync(&self) -> Result<SyncStatus> {
        let response = self.client.get(&self.url("v1/sync")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_gas(&self) -> Result<BlockGasSummary> {
        let response = self.client.get(&self.url("v1/gas")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn post_data(&self, data: &PostDataIntentV1Signed) -> Result<PostDataResponse> {
        let response = self
            .client
            .post(&self.url("v1/data"))
            .json(data)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data(&self) -> Result<Vec<DataIntentSummary>> {
        let response = self.client.get(&self.url("v1/data")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data_by_id(&self, id: &DataIntentId) -> Result<DataIntentFull> {
        let response = self
            .client
            .get(&self.url(&format!("v1/data/{}", id)))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_status_by_id(&self, id: DataIntentId) -> Result<DataIntentStatus> {
        let response = self
            .client
            .get(&self.url(&format!("v1/status/{}", id)))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_balance_by_address(&self, address: Address) -> Result<i128> {
        let response = self
            .client
            .get(&self.url(&format!("v1/balance/{}", address_to_hex_lowercase(address))))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    /// `path` must not start with /
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

const FACTOR_RESOLUTION: u128 = 1000;

#[derive(Debug)]
pub enum GasPreference {
    RelativeToHead(f64),
    Value(BlobGasPrice),
}

#[derive(Debug)]
pub enum NoncePreference {
    Timebased,
    Value(u64),
}
