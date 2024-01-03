use std::time::{SystemTime, UNIX_EPOCH};

use ethers::types::{Address, Signature};
use eyre::{bail, Result};
use reqwest::Response;

pub use crate::gas::get_blob_gasprice;

/// Post-process a reqwest response to handle non 2xx codes gracefully
pub(crate) async fn is_ok_response(response: Response) -> Result<Response> {
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

/// Return unix timestamp in milliseconds
pub(crate) fn unix_timestamps_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis() as u64
}

/// Return 0x prefixed hex representation of address (lowercase, not checksum)
pub(crate) fn address_to_hex_lowercase(addr: Address) -> String {
    vec_to_hex_0x_prefix(&addr.to_fixed_bytes())
}

/// Encode binary data to 0x prefixed hex
pub fn vec_to_hex_0x_prefix(v: &[u8]) -> String {
    format!("0x{}", hex::encode(v))
}

/// Deserialize ethers' Signature
pub(crate) fn deserialize_signature(signature: &[u8]) -> Result<Signature> {
    Ok(signature.try_into()?)
}
