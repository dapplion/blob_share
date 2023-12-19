use actix_web::http::header::AUTHORIZATION;
use actix_web::HttpRequest;
use ethers::types::{Address, Signature};
use eyre::{bail, eyre, Context, Result};
use reqwest::Response;
use std::cmp::PartialEq;
use std::fmt::{Debug, Display};
use std::ops::{Add, Div, Mul};
use std::time::{SystemTime, UNIX_EPOCH};

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
pub fn increase_by_min_percent<T>(value: T, percent: T) -> T
where
    T: Copy + Mul<Output = T> + Div<Output = T> + Add<Output = T> + PartialEq + From<u8>,
{
    let new_value = (percent * value) / T::from(100);
    if new_value == value {
        value + T::from(1)
    } else {
        value
    }
}

/// Post-process a reqwest response to handle non 2xx codes gracefully
pub async fn is_ok_response(response: Response) -> Result<Response> {
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

/// Return 0x prefixed hex representation of address (not checksum)
pub fn address_to_hex(addr: Address) -> String {
    format!("0x{}", hex::encode(addr.to_fixed_bytes()))
}

/// Deserialize ethers' Signature
pub fn deserialize_signature(signature: &[u8]) -> Result<Signature> {
    Ok(signature.try_into()?)
}

/// Return unix timestamp in milliseconds
pub fn unix_timestamps_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis()
}

/// Extract Bearer token from actix_web request, or return an error
pub fn extract_bearer_token(req: &HttpRequest) -> Result<String> {
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

pub fn parse_basic_auth(auth: &str) -> Result<BasicAuthentication> {
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
