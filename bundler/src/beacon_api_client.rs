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
            .get(self.url(&format!("eth/v1/beacon/blob_sidecars/{}", block_id.0)))
            .query(&query_values)
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_genesis(&self) -> Result<GetGenesisResponse> {
        let response = self
            .client
            .get(self.url("eth/v1/beacon/genesis"))
            .send()
            .await?;
        Ok(is_ok_response(response).await?.json().await?)
    }

    pub async fn get_spec(&self) -> Result<GetSpecResponse> {
        let response = self
            .client
            .get(self.url("eth/v1/config/spec"))
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- BeaconApiClient constructor ---

    #[test]
    fn new_valid_url() {
        let client = BeaconApiClient::new("http://localhost:5052");
        assert!(client.is_ok());
    }

    #[test]
    fn new_valid_url_with_trailing_slash() {
        let client = BeaconApiClient::new("http://localhost:5052/");
        assert!(client.is_ok());
    }

    #[test]
    fn new_invalid_url() {
        let client = BeaconApiClient::new("not-a-valid-url");
        assert!(client.is_err());
    }

    #[test]
    fn new_empty_url() {
        let client = BeaconApiClient::new("");
        assert!(client.is_err());
    }

    // --- URL building ---

    #[test]
    fn url_without_trailing_slash() {
        let client = BeaconApiClient::new("http://localhost:5052").unwrap();
        assert_eq!(
            client.url("eth/v1/beacon/genesis"),
            "http://localhost:5052/eth/v1/beacon/genesis"
        );
    }

    #[test]
    fn url_with_trailing_slash() {
        let client = BeaconApiClient::new("http://localhost:5052/").unwrap();
        assert_eq!(
            client.url("eth/v1/beacon/genesis"),
            "http://localhost:5052/eth/v1/beacon/genesis"
        );
    }

    #[test]
    fn url_blob_sidecars_path() {
        let client = BeaconApiClient::new("http://beacon:5052/").unwrap();
        let slot: Slot = 12345u64.into();
        let path = format!("eth/v1/beacon/blob_sidecars/{}", slot.0);
        assert_eq!(
            client.url(&path),
            "http://beacon:5052/eth/v1/beacon/blob_sidecars/12345"
        );
    }

    #[test]
    fn url_spec_path() {
        let client = BeaconApiClient::new("http://beacon:5052/").unwrap();
        assert_eq!(
            client.url("eth/v1/config/spec"),
            "http://beacon:5052/eth/v1/config/spec"
        );
    }

    // --- Slot type ---

    #[test]
    fn slot_from_u64() {
        let slot: Slot = 42u64.into();
        assert_eq!(slot.0, 42);
    }

    #[test]
    fn slot_from_zero() {
        let slot: Slot = 0u64.into();
        assert_eq!(slot.0, 0);
    }

    // --- GetGenesisResponse serde ---

    #[test]
    fn genesis_response_deserialize() {
        let json = r#"{"data":{"genesis_time":"1606824023"}}"#;
        let resp: GetGenesisResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.genesis_time, 1606824023);
    }

    #[test]
    fn genesis_response_roundtrip() {
        let resp = GetGenesisResponse {
            data: Genesis {
                genesis_time: 1606824023,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: GetGenesisResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp2.data.genesis_time, 1606824023);
    }

    #[test]
    fn genesis_response_zero_time() {
        let json = r#"{"data":{"genesis_time":"0"}}"#;
        let resp: GetGenesisResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.genesis_time, 0);
    }

    // --- GetSpecResponse serde ---

    #[test]
    fn spec_response_deserialize() {
        let json = r#"{"data":{"SLOTS_PER_EPOCH":"32","SECONDS_PER_SLOT":"12"}}"#;
        let resp: GetSpecResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.SLOTS_PER_EPOCH, 32);
        assert_eq!(resp.data.SECONDS_PER_SLOT, 12);
    }

    #[test]
    fn spec_response_roundtrip() {
        let resp = GetSpecResponse {
            data: Spec {
                SLOTS_PER_EPOCH: 32,
                SECONDS_PER_SLOT: 12,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: GetSpecResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp2.data.SLOTS_PER_EPOCH, 32);
        assert_eq!(resp2.data.SECONDS_PER_SLOT, 12);
    }

    #[test]
    fn spec_response_non_mainnet_values() {
        // Goerli/testnet can have different slot durations
        let json = r#"{"data":{"SLOTS_PER_EPOCH":"8","SECONDS_PER_SLOT":"6"}}"#;
        let resp: GetSpecResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.SLOTS_PER_EPOCH, 8);
        assert_eq!(resp.data.SECONDS_PER_SLOT, 6);
    }

    // --- GetBlobSidecarsResponse serde ---

    #[test]
    fn blob_sidecars_response_empty() {
        let json = r#"{"data":[]}"#;
        let resp: GetBlobSidecarsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.is_empty());
    }

    #[test]
    fn blob_sidecars_response_single_sidecar() {
        // Minimal valid sidecar: index quoted, blob/commitment/proof as 0x-prefixed hex
        let json = r#"{"data":[{
            "index":"0",
            "blob":"0xaabb",
            "kzg_commitment":"0xccdd",
            "kzg_proof":"0xeeff"
        }]}"#;
        let resp: GetBlobSidecarsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].index, 0);
        assert_eq!(resp.data[0].blob, vec![0xaa, 0xbb]);
        assert_eq!(resp.data[0].kzg_commitment, vec![0xcc, 0xdd]);
        assert_eq!(resp.data[0].kzg_proof, vec![0xee, 0xff]);
    }

    #[test]
    fn blob_sidecars_response_multiple_sidecars() {
        let json = r#"{"data":[
            {"index":"0","blob":"0x00","kzg_commitment":"0x01","kzg_proof":"0x02"},
            {"index":"3","blob":"0xff","kzg_commitment":"0xfe","kzg_proof":"0xfd"}
        ]}"#;
        let resp: GetBlobSidecarsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 2);
        assert_eq!(resp.data[0].index, 0);
        assert_eq!(resp.data[1].index, 3);
    }

    #[test]
    fn blob_sidecar_roundtrip() {
        let sidecar = BlobSidecar {
            index: 5,
            blob: vec![0x01, 0x02, 0x03],
            kzg_commitment: vec![0xaa, 0xbb],
            kzg_proof: vec![0xcc, 0xdd],
        };
        let resp = GetBlobSidecarsResponse {
            data: vec![sidecar],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let resp2: GetBlobSidecarsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp2.data.len(), 1);
        assert_eq!(resp2.data[0].index, 5);
        assert_eq!(resp2.data[0].blob, vec![0x01, 0x02, 0x03]);
        assert_eq!(resp2.data[0].kzg_commitment, vec![0xaa, 0xbb]);
        assert_eq!(resp2.data[0].kzg_proof, vec![0xcc, 0xdd]);
    }

    #[test]
    fn blob_sidecar_empty_bytes() {
        let json = r#"{"data":[{
            "index":"0",
            "blob":"0x",
            "kzg_commitment":"0x",
            "kzg_proof":"0x"
        }]}"#;
        let resp: GetBlobSidecarsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data[0].blob, Vec::<u8>::new());
        assert_eq!(resp.data[0].kzg_commitment, Vec::<u8>::new());
        assert_eq!(resp.data[0].kzg_proof, Vec::<u8>::new());
    }

    // --- quoted_u64 edge cases ---

    #[test]
    fn quoted_u64_large_value() {
        // Quoted u64 max value
        let json = r#"{"data":{"genesis_time":"18446744073709551615"}}"#;
        let resp: GetGenesisResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.genesis_time, u64::MAX);
    }

    #[test]
    fn quoted_u64_accepts_unquoted() {
        // The quoted_u64 serde also accepts raw numbers
        let json = r#"{"data":{"genesis_time":1606824023}}"#;
        let resp: GetGenesisResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.genesis_time, 1606824023);
    }
}
