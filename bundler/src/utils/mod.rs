use actix_web::http::header::AUTHORIZATION;
use actix_web::HttpRequest;
use ethers::types::{Address, Transaction, TxHash, H160, H256, U256};
use eyre::{bail, eyre, Context, Result};
use reqwest::Response;
use std::fmt::{self, Debug, Display};

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
pub(crate) fn e400<T: std::fmt::Debug + std::fmt::Display>(e: T) -> actix_web::Error
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

trait ResultExt<T, E>
where
    E: fmt::Display,
{
    fn prefix_err(self, prefix: &str) -> eyre::Result<T>;
}

impl<T, E> ResultExt<T, E> for Result<T, E>
where
    E: fmt::Display,
{
    fn prefix_err(self, prefix: &str) -> eyre::Result<T> {
        self.map_err(|e| eyre::eyre!("{}: {}", prefix, e.to_string().replace('\n', "; ")))
    }
}

pub(crate) fn wei_to_f64(wei: u128) -> f64 {
    let gwei = wei / 1_000_000_000;
    gwei as f64 / 1_000_000_000. // eth
}

#[cfg(test)]
mod tests {
    use ethers::types::Transaction;

    use crate::reth_fork::tx_eip4844::TxEip4844;

    use super::{increase_by_min_percent, tx_reth_to_ethers};

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
}
