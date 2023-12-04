use eyre::{bail, Result};
use reqwest::Response;

pub use crate::routes::{PostDataIntentV1, PostDataResponse, SenderDetails};

pub struct Client {
    base_url: String,
    client: reqwest::Client,
}

impl Client {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            client: reqwest::Client::new(),
        }
    }

    // Test health of server
    // $ curl -vv localhost:8000/health
    pub async fn health(&self) -> Result<u16> {
        let response = self
            .client
            .get(&format!("{}/v1/health", &self.base_url))
            .send()
            .await?;
        Ok(response.status().as_u16())
    }

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    pub async fn post_data(&self, data: &PostDataIntentV1) -> Result<PostDataResponse> {
        let response = self
            .client
            .post(&format!("{}/v1/data", &self.base_url))
            .json(data)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_data(&self) -> Result<Vec<PostDataIntentV1>> {
        let response = self
            .client
            .get(&format!("{}/v1/data", &self.base_url))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_sender(&self) -> Result<SenderDetails> {
        let response = self
            .client
            .get(&format!("{}/sender", &self.base_url))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }
}

async fn is_ok_response(response: Response) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        let status = response.status().as_u16();
        let body = match response.text().await {
            Ok(body) => body,
            Err(e) => format!("error getting error response text body: {}", e),
        };
        bail!("non-success response status {} body: {}", status, body);
    }
}
