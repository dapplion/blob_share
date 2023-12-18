use ethers::{signers::LocalWallet, types::Address};
use eyre::{eyre, Result};
use url::Url;

pub use crate::eth_provider::EthProvider;
use crate::routes::SyncStatus;
pub use crate::routes::{DataIntentStatus, PostDataIntentV1, PostDataResponse, SenderDetails};
use crate::utils::address_to_hex;
pub use crate::{data_intent::DataIntentId, DataIntent};
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
    ) -> Result<PostDataResponse> {
        let blob_gas_price_next_block = gas.blob_gas_price_next_block().await?;

        // TODO: customize, for now set gas price equal to next block
        // TODO: Close to genesis block the value is 1, which requires blobs to be perfectly full
        let max_cost_wei = blob_gas_price_next_block;

        self.post_data(
            &DataIntent::with_signature(wallet, data, max_cost_wei)
                .await?
                .into(),
        )
        .await
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

    pub async fn post_data(&self, data: &PostDataIntentV1) -> Result<PostDataResponse> {
        let response = self
            .client
            .post(&self.url("v1/data"))
            .json(data)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data(&self) -> Result<Vec<DataIntent>> {
        let response = self.client.get(&self.url("v1/data")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data_by_id(&self, id: &str) -> Result<DataIntent> {
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
            .get(&self.url(&format!("v1/balance/{}", address_to_hex(address))))
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
    FetchFromProvider(EthProvider),
}

impl GasPreference {
    pub async fn blob_gas_price_next_block(&self) -> Result<u128> {
        match self {
            GasPreference::FetchFromProvider(provider) => {
                // Choose data pricing correctly
                let head_block_number = provider.get_block_number().await?;
                let head_block = provider
                    .get_block(head_block_number)
                    .await?
                    .ok_or_else(|| eyre!("head block {head_block_number} should exist"))?;
                Ok(BlockGasSummary::from_block(&head_block)?.blob_gas_price_next_block())
            }
        }
    }
}
