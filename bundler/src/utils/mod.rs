use actix_web::http::header::AUTHORIZATION;
use actix_web::HttpRequest;
use ethers::types::{Address, Transaction, TxHash, H160, H256, U256};
use eyre::{bail, eyre, Context, Result};
use reqwest::Response;
use std::fmt::{Debug, Display};

use crate::reth_fork::tx_eip4844::TxEip4844;
use crate::reth_fork::tx_sidecar::BlobTransaction;

pub(crate) mod option_hex_vec;

// Return an opaque 500 while preserving the error root's cause for logging.
#[allow(dead_code)]
pub(crate) fn e500<T>(e: T) -> actix_web::Error
where
    T: Debug + Display + 'static,
{
    actix_web::error::ErrorInternalServerError(e)
}

// Return a 400 with the user-representation of the validation error as body.
// The error root cause is preserved for logging purposes.
pub(crate) fn e400<T>(e: T) -> actix_web::Error
where
    T: Debug + Display + 'static,
{
    actix_web::error::ErrorBadRequest(e)
}

/// Multiplies an integer value by `percent / 100`, if the resulting value is the same, returns the
/// value + 1.
pub fn increase_by_min_percent(value: u64, fraction: f64) -> u64 {
    ((value as f64) * fraction).ceil() as u64
}

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

/// Retrieves 'maxFeePerBlobGas' field from ethers transaction
pub fn get_max_fee_per_blob_gas(tx: &Transaction) -> Result<u128> {
    let max_fee_per_blob_gas: U256 = tx
        .other
        .get_deserialized("maxFeePerBlobGas")
        .ok_or_else(|| eyre!("not a type 3 tx, no max_fee_per_blob_gas"))??;
    Ok(max_fee_per_blob_gas.as_u128())
}

/// Return 0x prefixed hex representation of address (lowercase, not checksum)
pub(crate) fn address_to_hex_lowercase(addr: Address) -> String {
    vec_to_hex_0x_prefix(&addr.to_fixed_bytes())
}

/// Encode binary data to 0x prefixed hex
pub fn vec_to_hex_0x_prefix(v: &[u8]) -> String {
    format!("0x{}", hex::encode(v))
}

/// Decode 0x prefixed hex encoded string to `Vec<u8>`
pub fn hex_0x_prefix_to_vec(hex: &str) -> Result<Vec<u8>> {
    Ok(hex::decode(hex.trim_start_matches("0x"))?)
}

/// Convert `Vec<u8>` into ethers Address H160 type. Errors if v.len() != 20.
pub(crate) fn address_from_vec(v: &[u8]) -> Result<Address> {
    let fixed_vec: [u8; 20] = v
        .try_into()
        .map_err(|_| eyre!("address as vec not 20 bytes in len"))?;
    Ok(H160(fixed_vec))
}

/// Convert `Vec<u8>` into ethers TxHash H256 type. Errors if v.len() != 32.
pub fn txhash_from_vec(v: &[u8]) -> Result<TxHash> {
    let fixed_vec: [u8; 32] = v
        .try_into()
        .map_err(|_| eyre!("txhash as vec not 32 bytes in len"))?;
    Ok(H256(fixed_vec))
}

/// Compute the transaction hash from a serialized networking blob tx (pooled tx)
pub fn deserialize_blob_tx_pooled(serialized_networking_blob_tx: &[u8]) -> Result<BlobTransaction> {
    let mut blob_tx_networking = &serialized_networking_blob_tx[1..];
    Ok(BlobTransaction::decode_inner(&mut blob_tx_networking)?)
}

/// Convert a reth transaction to ethers
pub fn tx_reth_to_ethers(txr: &TxEip4844) -> Result<Transaction> {
    let mut tx = Transaction {
        chain_id: Some(txr.chain_id.into()),
        nonce: txr.nonce.into(),
        gas: txr.gas_limit.into(),
        max_fee_per_gas: Some(txr.max_fee_per_gas.into()),
        max_priority_fee_per_gas: Some(txr.max_priority_fee_per_gas.into()),
        to: Some(H160(txr.to.into())),
        input: txr.input.to_vec().into(),
        value: serde_json::from_value(serde_json::to_value(txr.value)?)?,
        ..Default::default()
    };
    tx.other.insert(
        "maxFeePerBlobGas".to_string(),
        serde_json::to_value(Into::<U256>::into(txr.max_fee_per_blob_gas))?,
    );
    Ok(tx)
}

/// Extract Bearer token from actix_web request, or return an error
pub(crate) fn extract_bearer_token(req: &HttpRequest) -> Result<String> {
    let auth = req
        .headers()
        .get(AUTHORIZATION)
        .ok_or_else(|| eyre!("Authorization header missing"))?
        .to_str()
        .wrap_err("invalid Authorization value format")?;

    if auth.starts_with("Bearer ") {
        Ok(auth.trim_start_matches("Bearer ").to_string())
    } else {
        bail!("Authorization value missing Bearer prefix");
    }
}

#[derive(Clone, Debug)]
pub struct BasicAuthentication {
    pub username: String,
    pub password: String,
}

pub(crate) fn parse_basic_auth(auth: &str) -> Result<BasicAuthentication> {
    let parts: Vec<&str> = auth.splitn(2, ':').collect();
    if parts.len() == 2 {
        Ok(BasicAuthentication {
            username: parts[0].to_string(),
            password: parts[1].to_string(),
        })
    } else {
        bail!("Invalid auth format. Use 'username:password'")
    }
}

pub(crate) fn wei_to_f64(wei: u128) -> f64 {
    let gwei = wei / 1_000_000_000;
    gwei as f64 / 1_000_000_000. // eth
}

#[cfg(test)]
mod tests {
    use ethers::types::{Transaction, U256};

    use crate::reth_fork::tx_eip4844::TxEip4844;

    use super::*;

    #[test]
    fn test_increase_by_min_percent() {
        // Bumps to more than 101% if low resolution
        assert_eq!(increase_by_min_percent(1, 1.01), 2);
        // Bump by some percents
        assert_eq!(increase_by_min_percent(100, 1.01), 101);
        assert_eq!(increase_by_min_percent(1000000000, 1.01), 1010000000);
        // Bump close to u64::MAX
        assert_eq!(
            increase_by_min_percent(10000000000000000000, 1.8),
            18000000000000000000
        );
        // Don't bump with fraction exactly 1
        assert_eq!(increase_by_min_percent(1, 1.), 1);
        assert_eq!(increase_by_min_percent(1000000000, 1.), 1000000000);
        // Precision loss
        assert_eq!(increase_by_min_percent(u64::MAX - 512, 1.), u64::MAX);
    }

    #[test]
    fn serde_ethers_transaction() {
        // ethers Transaction serde encodes all integers as quoted hex form
        let mut tx = Transaction::default();
        tx.chain_id = Some(69420.into());
        tx.nonce = 11.into();
        assert_eq!(
            serde_json::to_string(&tx).unwrap(),
            r#"{"hash":"0x0000000000000000000000000000000000000000000000000000000000000000","nonce":"0xb","blockHash":null,"blockNumber":null,"transactionIndex":null,"from":"0x0000000000000000000000000000000000000000","to":null,"value":"0x0","gasPrice":null,"gas":"0x0","input":"0x","v":"0x0","r":"0x0","s":"0x0","chainId":"0x10f2c"}"#
        );
    }

    #[test]
    fn test_tx_reth_to_ethers() {
        let mut txr = TxEip4844::default();
        txr.max_priority_fee_per_gas = 3000000000;
        tx_reth_to_ethers(&txr).unwrap();
    }

    // --- vec_to_hex_0x_prefix tests ---

    #[test]
    fn vec_to_hex_0x_prefix_empty() {
        assert_eq!(vec_to_hex_0x_prefix(&[]), "0x");
    }

    #[test]
    fn vec_to_hex_0x_prefix_single_byte() {
        assert_eq!(vec_to_hex_0x_prefix(&[0xff]), "0xff");
    }

    #[test]
    fn vec_to_hex_0x_prefix_multiple_bytes() {
        assert_eq!(
            vec_to_hex_0x_prefix(&[0xde, 0xad, 0xbe, 0xef]),
            "0xdeadbeef"
        );
    }

    #[test]
    fn vec_to_hex_0x_prefix_zero_bytes() {
        assert_eq!(vec_to_hex_0x_prefix(&[0x00, 0x00]), "0x0000");
    }

    // --- hex_0x_prefix_to_vec tests ---

    #[test]
    fn hex_0x_prefix_to_vec_with_prefix() {
        assert_eq!(
            hex_0x_prefix_to_vec("0xdeadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn hex_0x_prefix_to_vec_without_prefix() {
        assert_eq!(
            hex_0x_prefix_to_vec("deadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn hex_0x_prefix_to_vec_empty_with_prefix() {
        assert_eq!(hex_0x_prefix_to_vec("0x").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hex_0x_prefix_to_vec_empty_string() {
        assert_eq!(hex_0x_prefix_to_vec("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hex_0x_prefix_to_vec_invalid_hex() {
        assert!(hex_0x_prefix_to_vec("0xZZZZ").is_err());
    }

    #[test]
    fn hex_0x_prefix_to_vec_odd_length() {
        assert!(hex_0x_prefix_to_vec("0xabc").is_err());
    }

    // --- hex roundtrip ---

    #[test]
    fn hex_encode_decode_roundtrip() {
        let original = vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        let hex = vec_to_hex_0x_prefix(&original);
        let decoded = hex_0x_prefix_to_vec(&hex).unwrap();
        assert_eq!(original, decoded);
    }

    // --- address_to_hex_lowercase tests ---

    #[test]
    fn address_to_hex_lowercase_zero_address() {
        let addr = Address::zero();
        assert_eq!(
            address_to_hex_lowercase(addr),
            "0x0000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn address_to_hex_lowercase_nonzero() {
        let addr: Address = [0xAB; 20].into();
        // Should be lowercase hex
        assert_eq!(
            address_to_hex_lowercase(addr),
            "0xabababababababababababababababababababab"
        );
    }

    // --- address_from_vec tests ---

    #[test]
    fn address_from_vec_valid() {
        let bytes = vec![0xABu8; 20];
        let addr = address_from_vec(&bytes).unwrap();
        assert_eq!(addr, Address::from([0xAB; 20]));
    }

    #[test]
    fn address_from_vec_too_short() {
        let bytes = vec![0x01; 19];
        assert!(address_from_vec(&bytes).is_err());
    }

    #[test]
    fn address_from_vec_too_long() {
        let bytes = vec![0x01; 21];
        assert!(address_from_vec(&bytes).is_err());
    }

    #[test]
    fn address_from_vec_empty() {
        assert!(address_from_vec(&[]).is_err());
    }

    // --- txhash_from_vec tests ---

    #[test]
    fn txhash_from_vec_valid() {
        let bytes = vec![0xFFu8; 32];
        let hash = txhash_from_vec(&bytes).unwrap();
        assert_eq!(hash, H256([0xFF; 32]));
    }

    #[test]
    fn txhash_from_vec_too_short() {
        let bytes = vec![0x01; 31];
        assert!(txhash_from_vec(&bytes).is_err());
    }

    #[test]
    fn txhash_from_vec_too_long() {
        let bytes = vec![0x01; 33];
        assert!(txhash_from_vec(&bytes).is_err());
    }

    #[test]
    fn txhash_from_vec_empty() {
        assert!(txhash_from_vec(&[]).is_err());
    }

    // --- parse_basic_auth tests ---

    #[test]
    fn parse_basic_auth_valid() {
        let auth = parse_basic_auth("user:pass").unwrap();
        assert_eq!(auth.username, "user");
        assert_eq!(auth.password, "pass");
    }

    #[test]
    fn parse_basic_auth_password_with_colon() {
        // Password may contain colons — splitn(2) should keep them
        let auth = parse_basic_auth("user:pass:with:colons").unwrap();
        assert_eq!(auth.username, "user");
        assert_eq!(auth.password, "pass:with:colons");
    }

    #[test]
    fn parse_basic_auth_empty_password() {
        let auth = parse_basic_auth("user:").unwrap();
        assert_eq!(auth.username, "user");
        assert_eq!(auth.password, "");
    }

    #[test]
    fn parse_basic_auth_no_colon() {
        assert!(parse_basic_auth("nocolon").is_err());
    }

    #[test]
    fn parse_basic_auth_empty_string() {
        assert!(parse_basic_auth("").is_err());
    }

    // --- wei_to_f64 tests ---

    #[test]
    fn wei_to_f64_zero() {
        assert_eq!(wei_to_f64(0), 0.0);
    }

    #[test]
    fn wei_to_f64_one_eth() {
        // 1 ETH = 1e18 wei
        let one_eth: u128 = 1_000_000_000_000_000_000;
        assert!((wei_to_f64(one_eth) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn wei_to_f64_one_gwei() {
        // 1 gwei = 1e9 wei → 1e-9 ETH
        let one_gwei: u128 = 1_000_000_000;
        assert!((wei_to_f64(one_gwei) - 1e-9).abs() < 1e-18);
    }

    #[test]
    fn wei_to_f64_sub_gwei_truncated() {
        // Values below 1 gwei get truncated to 0 by integer division
        assert_eq!(wei_to_f64(999_999_999), 0.0);
    }

    // --- get_max_fee_per_blob_gas tests ---

    #[test]
    fn get_max_fee_per_blob_gas_present() {
        let mut tx = Transaction::default();
        // ethers U256 deserializes strings as hex
        tx.other
            .insert("maxFeePerBlobGas".to_string(), serde_json::json!("0x64"));
        let fee = get_max_fee_per_blob_gas(&tx).unwrap();
        assert_eq!(fee, 100);
    }

    #[test]
    fn get_max_fee_per_blob_gas_missing() {
        let tx = Transaction::default();
        assert!(get_max_fee_per_blob_gas(&tx).is_err());
    }

    #[test]
    fn get_max_fee_per_blob_gas_hex_string() {
        let mut tx = Transaction::default();
        // ethers U256 can deserialize from hex string
        tx.other
            .insert("maxFeePerBlobGas".to_string(), serde_json::json!("0xff"));
        let fee = get_max_fee_per_blob_gas(&tx).unwrap();
        assert_eq!(fee, 255);
    }

    // --- extract_bearer_token tests ---

    #[test]
    fn extract_bearer_token_valid() {
        let req = actix_web::test::TestRequest::default()
            .insert_header((AUTHORIZATION, "Bearer my-secret-token"))
            .to_http_request();
        let token = extract_bearer_token(&req).unwrap();
        assert_eq!(token, "my-secret-token");
    }

    #[test]
    fn extract_bearer_token_missing_header() {
        let req = actix_web::test::TestRequest::default().to_http_request();
        assert!(extract_bearer_token(&req).is_err());
    }

    #[test]
    fn extract_bearer_token_wrong_scheme() {
        let req = actix_web::test::TestRequest::default()
            .insert_header((AUTHORIZATION, "Basic dXNlcjpwYXNz"))
            .to_http_request();
        assert!(extract_bearer_token(&req).is_err());
    }

    #[test]
    fn extract_bearer_token_empty_token() {
        let req = actix_web::test::TestRequest::default()
            .insert_header((AUTHORIZATION, "Bearer "))
            .to_http_request();
        let token = extract_bearer_token(&req).unwrap();
        assert_eq!(token, "");
    }

    // --- tx_reth_to_ethers extended tests ---

    #[test]
    fn tx_reth_to_ethers_preserves_chain_id() {
        let mut txr = TxEip4844::default();
        txr.chain_id = 42161;
        let tx = tx_reth_to_ethers(&txr).unwrap();
        assert_eq!(tx.chain_id, Some(U256::from(42161)));
    }

    #[test]
    fn tx_reth_to_ethers_preserves_nonce() {
        let mut txr = TxEip4844::default();
        txr.nonce = 999;
        let tx = tx_reth_to_ethers(&txr).unwrap();
        assert_eq!(tx.nonce, U256::from(999));
    }

    #[test]
    fn tx_reth_to_ethers_preserves_gas_fields() {
        let mut txr = TxEip4844::default();
        txr.gas_limit = 100_000;
        txr.max_fee_per_gas = 30_000_000_000;
        txr.max_priority_fee_per_gas = 2_000_000_000;
        txr.max_fee_per_blob_gas = 500;
        let tx = tx_reth_to_ethers(&txr).unwrap();
        assert_eq!(tx.gas, U256::from(100_000));
        assert_eq!(tx.max_fee_per_gas, Some(U256::from(30_000_000_000u64)));
        assert_eq!(
            tx.max_priority_fee_per_gas,
            Some(U256::from(2_000_000_000u64))
        );
        // max_fee_per_blob_gas is stored in the `other` field
        let blob_fee = get_max_fee_per_blob_gas(&tx).unwrap();
        assert_eq!(blob_fee, 500);
    }

    #[test]
    fn tx_reth_to_ethers_preserves_input() {
        let mut txr = TxEip4844::default();
        txr.input = vec![0xde, 0xad, 0xbe, 0xef].into();
        let tx = tx_reth_to_ethers(&txr).unwrap();
        assert_eq!(tx.input.to_vec(), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn tx_reth_to_ethers_preserves_to_address() {
        let mut txr = TxEip4844::default();
        txr.to = alloy_primitives::Address::from([0xAB; 20]);
        let tx = tx_reth_to_ethers(&txr).unwrap();
        assert_eq!(tx.to, Some(H160([0xAB; 20])));
    }
}
