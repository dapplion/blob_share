use ethers::{
    signers::{LocalWallet, Signer},
    types::Address,
};
use eyre::{Context, Result};
use url::Url;

use crate::{
    types::{
        BlobGasPrice, CancelDataIntentSigned, DataIntentFull, DataIntentId, DataIntentStatus,
        DataIntentSummary, GasResponse, HistoryResponse, PostDataIntentV1, PostDataIntentV1Signed,
        PostDataResponse, SenderDetails, SyncStatus,
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
        let max_blob_gas_price = match gas {
            GasPreference::RelativeToHead(factor) => {
                let gas_response = self.get_gas().await.wrap_err("get_gas")?;
                let blob_gas_price_next_block = gas_response.recommended.max_blob_gas_price;
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
        let response = self.client.get(self.url("v1/health")).send().await?;
        is_ok_response(response).await?;
        Ok(())
    }

    pub async fn get_sender(&self) -> Result<SenderDetails> {
        let response = self.client.get(self.url("v1/sender")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_sync(&self) -> Result<SyncStatus> {
        let response = self.client.get(self.url("v1/sync")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_gas(&self) -> Result<GasResponse> {
        let response = self.client.get(self.url("v1/gas")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn post_data(&self, data: &PostDataIntentV1Signed) -> Result<PostDataResponse> {
        let response = self
            .client
            .post(self.url("v1/data"))
            .json(data)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data(&self) -> Result<Vec<DataIntentSummary>> {
        let response = self.client.get(self.url("v1/data")).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data_by_id(&self, id: &DataIntentId) -> Result<DataIntentFull> {
        let response = self
            .client
            .get(self.url(&format!("v1/data/{}", id)))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_status_by_id(&self, id: DataIntentId) -> Result<DataIntentStatus> {
        let response = self
            .client
            .get(self.url(&format!("v1/status/{}", id)))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_balance_by_address(&self, address: Address) -> Result<i128> {
        let response = self
            .client
            .get(self.url(&format!("v1/balance/{}", address_to_hex_lowercase(address))))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_history(
        &self,
        address: Address,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<HistoryResponse> {
        let mut url = self.url(&format!("v1/history/{}", address_to_hex_lowercase(address)));
        let mut params = Vec::new();
        if let Some(limit) = limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(offset) = offset {
            params.push(format!("offset={offset}"));
        }
        if !params.is_empty() {
            url = format!("{}?{}", url, params.join("&"));
        }
        let response = self.client.get(&url).send().await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn cancel_data_intent(&self, cancel: &CancelDataIntentSigned) -> Result<()> {
        let response = self
            .client
            .delete(self.url(&format!("v1/data/{}", cancel.id)))
            .json(cancel)
            .send()
            .await?;
        is_ok_response(response).await?;
        Ok(())
    }

    pub async fn cancel_data_intent_with_wallet(
        &self,
        wallet: &LocalWallet,
        id: DataIntentId,
    ) -> Result<()> {
        let cancel = CancelDataIntentSigned::with_signature(wallet, wallet.address(), id).await?;
        self.cancel_data_intent(&cancel).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_new_valid_url() {
        let client = Client::new("http://localhost:8080/");
        assert!(client.is_ok());
    }

    #[test]
    fn client_new_valid_url_no_trailing_slash() {
        let client = Client::new("http://localhost:8080");
        assert!(client.is_ok());
    }

    #[test]
    fn client_new_invalid_url() {
        let client = Client::new("not-a-url");
        assert!(client.is_err());
    }

    #[test]
    fn client_url_helper_with_trailing_slash() {
        let client = Client::new("http://localhost:8080/").unwrap();
        assert_eq!(client.url("v1/health"), "http://localhost:8080/v1/health");
    }

    #[test]
    fn client_url_helper_without_trailing_slash() {
        let client = Client::new("http://localhost:8080").unwrap();
        // Url::parse normalizes the path
        assert_eq!(client.url("v1/health"), "http://localhost:8080/v1/health");
    }

    #[test]
    fn client_url_helper_with_path_prefix() {
        let client = Client::new("http://localhost:8080/api/").unwrap();
        assert_eq!(
            client.url("v1/health"),
            "http://localhost:8080/api/v1/health"
        );
    }

    #[test]
    fn gas_preference_value_stores_price() {
        let pref = GasPreference::Value(42);
        match pref {
            GasPreference::Value(v) => assert_eq!(v, 42),
            _ => panic!("expected Value variant"),
        }
    }

    #[test]
    fn gas_preference_relative_stores_factor() {
        let pref = GasPreference::RelativeToHead(1.5);
        match pref {
            GasPreference::RelativeToHead(f) => assert!((f - 1.5).abs() < f64::EPSILON),
            _ => panic!("expected RelativeToHead variant"),
        }
    }

    #[test]
    fn nonce_preference_value_stores_nonce() {
        let pref = NoncePreference::Value(12345);
        match pref {
            NoncePreference::Value(v) => assert_eq!(v, 12345),
            _ => panic!("expected Value variant"),
        }
    }

    #[test]
    fn factor_resolution_constant() {
        assert_eq!(FACTOR_RESOLUTION, 1000);
    }

    #[test]
    fn client_url_format_data_by_id() {
        let client = Client::new("http://localhost:8080/").unwrap();
        let id = "c4f1bdd0-3331-4470-b427-28a2c514f483";
        assert_eq!(
            client.url(&format!("v1/data/{}", id)),
            format!("http://localhost:8080/v1/data/{}", id)
        );
    }

    #[test]
    fn client_url_format_balance() {
        let client = Client::new("http://localhost:8080/").unwrap();
        let addr = "0x0000000000000000000000000000000000000001";
        assert_eq!(
            client.url(&format!("v1/balance/{}", addr)),
            format!("http://localhost:8080/v1/balance/{}", addr)
        );
    }
}
