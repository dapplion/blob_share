use ethers::types::Address;
use eyre::Result;
use url::Url;

pub use crate::routes::{DataIntentStatus, PostDataIntentV1, PostDataResponse, SenderDetails};
use crate::utils::is_ok_response;
pub use crate::{data_intent::DataIntentId, DataIntent};

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

    pub async fn health(&self) -> Result<()> {
        let response = self.client.get(&self.url("v1/health")).send().await?;
        is_ok_response(response).await?;
        Ok(())
    }

    pub async fn get_sender(&self) -> Result<SenderDetails> {
        let response = self.client.get(&self.url("v1/sender")).send().await?;
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
            .get(&self.url(&format!("v1/balance/{}", address)))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    /// `path` must not start with /
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}
