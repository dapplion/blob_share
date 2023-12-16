use eyre::Result;
use serde::{Deserialize, Serialize};
use serde_utils::{hex_vec, quoted_u64};
use url::Url;

use crate::utils::is_ok_response;

pub struct BeaconApiClient {
    base_url: Url,
    client: reqwest::Client,
}

impl BeaconApiClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            base_url: Url::parse(base_url)?,
            client: reqwest::Client::new(),
        })
    }

    /// <https://github.com/ethereum/beacon-APIs/blob/master/apis/beacon/blob_sidecars/blob_sidecars.yaml>
    pub async fn get_blob_sidecars(
        &self,
        block_id: Slot,
        indices: Option<&[usize]>,
    ) -> Result<GetBlobSidecarsResponse> {
        let query_values = if let Some(indices) = indices {
            let query_value = indices
                .iter()
                .map(|&num| num.to_string())
                .collect::<Vec<String>>()
                .join(",");
            vec![("indices", query_value)]
        } else {
            vec![]
        };

        let response = self
            .client
            .get(&self.url(&format!("eth/v1/beacon/blob_sidecars/{}", block_id.0)))
            .query(&query_values)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_genesis(&self) -> Result<GetGenesisResponse> {
        let response = self
            .client
            .get(&self.url("eth/v1/beacon/genesis"))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_spec(&self) -> Result<GetSpecResponse> {
        let response = self
            .client
            .get(&self.url("eth/v1/config/spec"))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    /// `path` must not start with /
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

pub struct Slot(u64);

impl From<u64> for Slot {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GetSpecResponse {
    pub data: Spec,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Spec {
    #[serde(with = "quoted_u64")]
    pub SLOTS_PER_EPOCH: u64,
    #[serde(with = "quoted_u64")]
    pub SECONDS_PER_SLOT: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GetGenesisResponse {
    pub data: Genesis,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Genesis {
    #[serde(with = "quoted_u64")]
    pub genesis_time: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GetBlobSidecarsResponse {
    pub data: Vec<BlobSidecar>,
}

/// <https://github.com/ethereum/beacon-APIs/blob/6559c852193b14a33ddcb1ba770db29d62a52878/types/deneb/blob_sidecar.yaml#L16>
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlobSidecar {
    #[serde(with = "quoted_u64")]
    pub index: u64,
    /// A blob is `FIELD_ELEMENTS_PER_BLOB * size_of(BLSFieldElement) = 4096 * 32 = 131072` bytes (`DATA`) representing a SSZ-encoded Blob as defined in Deneb
    #[serde(with = "hex_vec")]
    pub blob: Vec<u8>,
    /// A G1 curve point. Same as BLS standard \"is valid pubkey\" check but also allows `0x00..00` for point-at-infinity
    #[serde(with = "hex_vec")]
    pub kzg_commitment: Vec<u8>,
    /// A G1 curve point. Used for verifying that the `KZGCommitment` for a given `Blob` is correct.
    #[serde(with = "hex_vec")]
    pub kzg_proof: Vec<u8>,
}
