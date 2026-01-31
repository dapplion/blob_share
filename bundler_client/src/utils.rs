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

#[cfg(test)]
mod tests {
    use super::*;

    // -- unix_timestamps_millis --

    #[test]
    fn unix_timestamps_millis_returns_positive_value() {
        let ts = unix_timestamps_millis();
        assert!(ts > 0);
    }

    #[test]
    fn unix_timestamps_millis_is_reasonable() {
        // Should be after 2024-01-01 in milliseconds
        let ts = unix_timestamps_millis();
        let jan_2024_ms: u64 = 1_704_067_200_000;
        assert!(
            ts > jan_2024_ms,
            "timestamp {ts} should be after 2024-01-01"
        );
    }

    // -- address_to_hex_lowercase --

    #[test]
    fn address_to_hex_lowercase_zero() {
        let addr = Address::zero();
        let hex = address_to_hex_lowercase(addr);
        assert_eq!(hex, "0x0000000000000000000000000000000000000000");
    }

    #[test]
    fn address_to_hex_lowercase_non_zero() {
        let addr = Address::from([0xAB; 20]);
        let hex = address_to_hex_lowercase(addr);
        assert_eq!(hex, "0xabababababababababababababababababababab");
    }

    #[test]
    fn address_to_hex_lowercase_is_lowercase() {
        let addr = Address::from([0xFF; 20]);
        let hex = address_to_hex_lowercase(addr);
        // Should not contain uppercase hex chars
        assert_eq!(hex, hex.to_lowercase());
    }

    // -- vec_to_hex_0x_prefix --

    #[test]
    fn vec_to_hex_0x_prefix_empty() {
        assert_eq!(vec_to_hex_0x_prefix(&[]), "0x");
    }

    #[test]
    fn vec_to_hex_0x_prefix_single_byte() {
        assert_eq!(vec_to_hex_0x_prefix(&[0xFF]), "0xff");
    }

    #[test]
    fn vec_to_hex_0x_prefix_multiple_bytes() {
        assert_eq!(
            vec_to_hex_0x_prefix(&[0xDE, 0xAD, 0xBE, 0xEF]),
            "0xdeadbeef"
        );
    }

    #[test]
    fn vec_to_hex_0x_prefix_zero_bytes() {
        assert_eq!(vec_to_hex_0x_prefix(&[0x00, 0x00]), "0x0000");
    }

    // -- deserialize_signature --

    #[test]
    fn deserialize_signature_valid_65_bytes() {
        // A valid 65-byte signature (r: 32 bytes, s: 32 bytes, v: 1 byte)
        let mut sig_bytes = vec![0u8; 65];
        sig_bytes[0] = 1; // r[0]
        sig_bytes[32] = 2; // s[0]
        sig_bytes[64] = 27; // v
        let result = deserialize_signature(&sig_bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn deserialize_signature_too_short() {
        let sig_bytes = vec![0u8; 32];
        let result = deserialize_signature(&sig_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_signature_too_long() {
        let sig_bytes = vec![0u8; 66];
        let result = deserialize_signature(&sig_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_signature_empty() {
        let result = deserialize_signature(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_signature_roundtrip() {
        // Create a signature, convert to bytes, then back
        let mut sig_bytes = vec![0u8; 65];
        sig_bytes[31] = 1; // r = 1
        sig_bytes[63] = 2; // s = 2
        sig_bytes[64] = 27; // v = 27
        let sig = deserialize_signature(&sig_bytes).unwrap();
        let sig_back: Vec<u8> = sig.into();
        assert_eq!(sig_back.len(), 65);
    }
}
