use ethers::{
    signers::{LocalWallet, Signer},
    types::Address,
};
use eyre::{eyre, Result};
use url::Url;

pub use crate::data_intent_tracker::{
    DataIntentDbRowFull, DataIntentDbRowSummary, DataIntentSummary,
};
pub use crate::eth_provider::EthProvider;
pub use crate::routes::{DataIntentStatus, PostDataIntentV1, PostDataResponse, SenderDetails};
use crate::{
    data_intent::BlobGasPrice, routes::PostDataIntentV1Signed, utils::address_to_hex_lowercase,
};
pub use crate::{data_intent::DataIntentId, DataIntent};
use crate::{routes::SyncStatus, utils::unix_timestamps_millis};
use crate::{utils::is_ok_response, BlockGasSummary};

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
        let max_blob_gas_price = gas.max_blob_gas_price().await?;

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

    pub async fn get_data_by_id(&self, id: &DataIntentId) -> Result<DataIntentDbRowFull> {
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

    pub async fn get_last_seen_nonce_by_address(&self, address: Address) -> Result<Option<u128>> {
        let response = self
            .client
            .get(&self.url(&format!(
                "v1/last_seen_nonce/{}",
                address_to_hex_lowercase(address)
            )))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    /// `path` must not start with /
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

pub enum GasPreference {
    RelativeToHead(EthProvider, f64),
}

const FACTOR_RESOLUTION: u128 = 1000;

impl GasPreference {
    pub async fn max_blob_gas_price(&self) -> Result<BlobGasPrice> {
        match self {
            GasPreference::RelativeToHead(provider, factor_to_next_block) => {
                // Choose data pricing correctly
                let head_block_number = provider.get_block_number().await?;
                let head_block = provider
                    .get_block(head_block_number)
                    .await?
                    .ok_or_else(|| eyre!("head block {head_block_number} should exist"))?;
                let blob_gas_price_next_block =
                    BlockGasSummary::from_block(&head_block)?.blob_gas_price_next_block();

                Ok(if *factor_to_next_block == 1.0 {
                    blob_gas_price_next_block as BlobGasPrice
                } else {
                    (((FACTOR_RESOLUTION as f64 * factor_to_next_block) as u128
                        * blob_gas_price_next_block)
                        / FACTOR_RESOLUTION) as BlobGasPrice
                })
            }
        }
    }
}

pub enum NoncePreference {
    Timebased,
    Value(u64),
}
